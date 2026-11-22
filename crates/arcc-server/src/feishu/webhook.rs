//! Feishu webhook handler — receives event callbacks from Feishu Open API.
//!
//! Supports three event types:
//! - **URL verification**: echoes the `challenge` field (one-time setup).
//! - **Message events** (`im.message.receive_v1`): forwards the message to
//!   the shared IM pipeline, which calls the LLM and sends the response.
//! - **Card action events** (`card.action.trigger`): processes approve/deny
//!   button clicks on interactive confirmation cards.
//!
//! Also exports a `send` handler for proactively sending messages to Feishu.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{error, info, warn};

use arcc_core::context::SharedContext;

use super::card;
use crate::im::{ChatEvent, chat_queue};

/// Feishu schema 2.0 event header.
#[derive(Debug, Deserialize)]
pub struct HeaderV2 {
    pub token: Option<String>,
    #[serde(rename = "event_type")]
    pub event_type: Option<String>,
}

/// Feishu schema 2.0 event payload (header is optional for URL verification compat).
#[derive(Debug, Default, Deserialize)]
pub struct EventV2 {
    pub schema: Option<String>,
    #[serde(default)]
    pub header: Option<HeaderV2>,
    #[serde(default)]
    pub event: serde_json::Value,
}

/// Card action payload.
#[derive(Debug, Deserialize)]
pub struct CardActionPayload {
    pub value: Option<serde_json::Value>,
    pub tag: Option<String>,
    #[serde(rename = "message_id")]
    pub message_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WebhookResponse {
    pub code: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
}

/// POST /feishu/webhook — Feishu schema 2.0 only.
pub async fn handler(State(ctx): State<SharedContext>, body: String) -> Json<WebhookResponse> {
    info!(raw_body = %body, "feishu webhook received");

    // 1. Parse the schema 2.0 payload.
    let payload: EventV2 = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            warn!(err = %e, "failed to parse feishu webhook payload");
            return Json(WebhookResponse {
                code: 400,
                challenge: None,
                msg: Some(format!("invalid json: {e}")),
            });
        }
    };

    // 2. URL verification — challenge lives at top-level or event.challenge.
    if let Some(challenge) = payload.event.get("challenge").and_then(|c| c.as_str()) {
        return Json(WebhookResponse {
            code: 0,
            challenge: Some(challenge.to_owned()),
            msg: None,
        });
    }

    let header = match payload.header {
        Some(h) => h,
        None => {
            warn!("feishu webhook missing header (possibly legacy challenge-only request)");
            return Json(WebhookResponse {
                code: 0,
                challenge: None,
                msg: Some("ok".into()),
            });
        }
    };

    // 3. Token validation.
    let expected_token = &ctx.storage.config.feishu.verification_token;
    if !expected_token.is_empty() {
        let actual = header.token.as_deref().unwrap_or("");
        if actual != expected_token {
            warn!(
                expected = %expected_token,
                actual = %actual,
                "feishu webhook token mismatch"
            );
            return Json(WebhookResponse {
                code: 403,
                challenge: None,
                msg: Some("invalid token".into()),
            });
        }
    }

    // 4. Dispatch by event type.
    let event_type = header.event_type.as_deref().unwrap_or("");

    match event_type {
        "im.message.receive_v1" => {
            handle_message_event(&ctx, &payload.event).await;
        }
        "card.action.trigger" => {
            let action: CardActionPayload =
                serde_json::from_value(payload.event.get("action").cloned().unwrap_or_default())
                    .unwrap_or(CardActionPayload {
                        value: None,
                        tag: None,
                        message_id: None,
                    });
            handle_card_action(&ctx, &action).await;
        }
        other => {
            info!(event_type = %other, "unknown feishu webhook event type");
        }
    }

    Json(WebhookResponse {
        code: 0,
        challenge: None,
        msg: None,
    })
}

/// Handle `im.message.receive_v1` — user sent a message to the bot.
async fn handle_message_event(ctx: &SharedContext, event: &serde_json::Value) {
    let message = match event["message"].as_object() {
        Some(m) => m,
        None => {
            warn!("feishu message event missing message object");
            return;
        }
    };

    let chat_id = message["chat_id"].as_str().unwrap_or("").to_owned();
    let message_id = message["message_id"].as_str().unwrap_or("").to_owned();
    let msg_type = message["message_type"].as_str().unwrap_or("");
    let chat_type = event["message"]["chat_type"]
        .as_str()
        .unwrap_or("p2p")
        .to_owned();
    let open_id = event["sender"]["sender_id"]["open_id"]
        .as_str()
        .unwrap_or("")
        .to_owned();

    if msg_type != "text" {
        info!(msg_type, "ignoring non-text feishu message");
        return;
    }

    // Group chat: only respond when the bot is @mentioned.
    if chat_type == "group" {
        let mentions = event["message"]["mentions"].as_array();
        let has_mention = mentions.is_some_and(|m| !m.is_empty());
        if !has_mention {
            info!("ignoring group message without @mention");
            return;
        }
    }

    let content_raw = message["content"].as_str().unwrap_or("{}");
    let content_val: serde_json::Value = serde_json::from_str(content_raw).unwrap_or_default();
    let mut user_text = content_val["text"].as_str().unwrap_or("").trim().to_owned();

    // Strip @mention placeholders (e.g. @_user_1) so the AI only sees the
    // actual user message.
    if let Some(mentions) = event["message"]["mentions"].as_array() {
        for m in mentions {
            if let Some(key) = m["key"].as_str() {
                user_text = user_text.replace(key, "").trim().to_owned();
            }
        }
    }

    if user_text.is_empty() {
        warn!("empty text content in feishu message");
        return;
    }

    info!(chat_id = %chat_id, chat_type = %chat_type, len = user_text.len(), "feishu message received");

    let (reply_id, reply_id_type) = if chat_type == "group" {
        (chat_id.clone(), "chat_id")
    } else {
        (open_id.clone(), "open_id")
    };

    // Enqueue to the per-chat processing pipeline — serializes messages
    // so concurrent writes never corrupt the session VecDeque.
    chat_queue()
        .enqueue(
            ctx.clone(),
            ChatEvent {
                platform: "feishu",
                chat_id,
                chat_type,
                user_id: open_id,
                message_id,
                reply_id,
                reply_id_type: reply_id_type.to_owned(),
                session_webhook: None,
                user_text,
            },
        )
        .await;
}

/// Handle `card.action.trigger` — user clicked a button on an interactive card.
async fn handle_card_action(ctx: &SharedContext, action: &CardActionPayload) {
    let value = match action.value.as_ref() {
        Some(v) => v,
        None => {
            warn!("card action missing value");
            return;
        }
    };

    let action_type = value["action"].as_str().unwrap_or("").to_owned();
    let operation = value["operation"].as_str().unwrap_or("?").to_owned();
    let message_id = match action.message_id.as_ref() {
        Some(id) => id.clone(),
        None => {
            warn!("card action missing message_id");
            return;
        }
    };

    info!(action = %action_type, op = %operation, "feishu card action received");

    // 1. Audit the decision.
    use arcc_storage::audit::types::{AuditEvent, ConfirmDecision};
    let decision = if action_type == "approve" {
        ConfirmDecision::Approved
    } else {
        ConfirmDecision::Denied
    };
    ctx.storage.audit.write(&AuditEvent::HumanConfirm {
        ts: chrono::Utc::now().to_rfc3339(),
        session: "feishu".into(),
        action: operation.clone(),
        decision,
        user: "feishu".into(),
    });

    // 2. Build an updated card showing the result.
    let updated_card = if action_type == "approve" {
        card::build_approved_card(&operation, "Approved via Feishu card.")
    } else {
        card::build_denied_card(&operation, "Denied via Feishu card.")
    };

    // 3. Update the message to replace buttons with status text.
    if let Some(client) = ctx.feishu_client.as_ref()
        && let Err(e) = client.update_message(&message_id, updated_card).await
    {
        error!(err = %e, "failed to update feishu card");
    }
}

// ── Proactive send (for testing / notifications) ──────────────────

#[derive(Debug, Deserialize)]
pub struct SendRequest {
    pub open_id: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct SendResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// POST /feishu/send — proactively send a text message to a Feishu user.
pub async fn send_handler(
    State(ctx): State<SharedContext>,
    Json(body): Json<SendRequest>,
) -> Json<SendResponse> {
    let client = match ctx.feishu_client.as_ref() {
        Some(c) => c,
        None => {
            return Json(SendResponse {
                success: false,
                error: Some("Feishu client not configured".into()),
            });
        }
    };

    match client
        .send_message(&body.open_id, json!({"text": body.text}), "text")
        .await
    {
        Ok(()) => Json(SendResponse {
            success: true,
            error: None,
        }),
        Err(e) => Json(SendResponse {
            success: false,
            error: Some(e.to_string()),
        }),
    }
}
