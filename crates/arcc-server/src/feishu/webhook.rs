//! Feishu webhook handler — receives event callbacks from Feishu Open API.
//!
//! Supports three event types:
//! - **URL verification**: echoes the `challenge` field (one-time setup).
//! - **Message events** (`im.message.receive_v1`): creates a session, calls the
//!   LLM, and sends the response back as a Feishu text message.
//! - **Card action events** (`card.action.trigger`): processes approve/deny
//!   button clicks on interactive confirmation cards.
//!
//! Also exports a `send` handler for proactively sending messages to Feishu
//! (useful for testing and notifications).

use axum::{
    extract::State,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{error, info, warn};

use arcc_core::context::SharedContext;
use arcc_core::model::types::{ChatMessage, ChatRequest};
use arcc_core::tools;

use super::card;

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
pub async fn handler(
    State(ctx): State<SharedContext>,
    body: String,
) -> Json<WebhookResponse> {
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
            let action: CardActionPayload = serde_json::from_value(
                payload.event.get("action").cloned().unwrap_or_default(),
            )
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
    let chat_type = event["message"]["chat_type"].as_str().unwrap_or("p2p").to_owned();
    let open_id = event["sender"]["sender_id"]["open_id"]
        .as_str()
        .unwrap_or("")
        .to_owned();

    // Only handle text messages.
    if msg_type != "text" {
        info!(msg_type, "ignoring non-text feishu message");
        return;
    }

    let content_raw = message["content"].as_str().unwrap_or("{}");
    let content_val: serde_json::Value = serde_json::from_str(content_raw).unwrap_or_default();
    let user_text = content_val["text"].as_str().unwrap_or("").trim().to_owned();

    if user_text.is_empty() {
        warn!("empty text content in feishu message");
        return;
    }

    info!(chat_id = %chat_id, chat_type = %chat_type, len = user_text.len(), "feishu message received");

    // Spawn background task so the webhook returns 200 immediately.
    let ctx = ctx.clone();
    tokio::spawn(async move {
        process_feishu_chat(&ctx, &chat_id, &chat_type, &open_id, &message_id, &user_text).await;
    });
}

/// Process a Feishu chat message: LLM → tool calls → LLM → send response.
async fn process_feishu_chat(
    ctx: &SharedContext,
    chat_id: &str,
    chat_type: &str,
    open_id: &str,
    _message_id: &str,
    user_text: &str,
) {
    // 1. Pick a provider.
    let provider = match ctx.providers.pick(user_text.len(), true) {
        Some(p) => p.clone(),
        None => {
            warn!("no provider available for feishu chat");
            send_fallback(ctx, chat_id, chat_type, open_id, "Service unavailable: no model provider").await;
            return;
        }
    };

    // 2. Create/reuse session (keyed by chat_id).
    let session = ctx.sessions.create(chat_id, "feishu").await;

    // 3. Persist user message.
    let user_tokens = provider.count_tokens(user_text);
    {
        let mut s = session.write().await;
        s.push_message(
            ChatMessage {
                role: "user".into(),
                content: user_text.to_owned(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            user_tokens,
        );
    }

    // 4. Build initial messages: system prompt + memory + current user text.
    let system_msg = arcc_core::model::prompts::templates::server().to_chat_message();
    let memory_context = ctx.memory.format_for_context(chat_id);

    let mut messages = Vec::with_capacity(3);
    messages.push(system_msg);
    if !memory_context.is_empty() {
        messages.push(ChatMessage {
            role: "system".into(),
            content: memory_context,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
    }
    messages.push(ChatMessage {
        role: "user".into(),
        content: user_text.to_owned(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    // 5. Two-phase tool calling loop.
    let tool_def = tools::command_tool_definition();
    let temperature = ctx.storage.config.model.temperature;
    let max_tokens = ctx.storage.config.model.max_output_tokens;
    let mut phase = 1;

    let reply_text = loop {
        let has_tools = phase == 1;
        let req = ChatRequest {
            model: provider.model_name().to_owned(),
            messages: messages.clone(),
            tools: if has_tools { Some(vec![tool_def.clone()]) } else { None },
            tool_choice: if has_tools { Some(serde_json::json!("auto")) } else { None },
            temperature: Some(temperature),
            max_tokens: Some(max_tokens),
            stream: false,
            thinking_mode: None,
            reasoning_effort: None,
        };

        let response = match provider.chat(req).await {
            Ok(r) => r,
            Err(e) => {
                error!(err = %e, "LLM call failed for feishu message");
                send_fallback(ctx, chat_id, chat_type, open_id, "Sorry, I encountered an error processing your message.").await;
                return;
            }
        };

        let msg = response.message;

        // Persist assistant response.
        let asst_tokens = provider.count_tokens(&msg.content);
        {
            let mut s = session.write().await;
            s.push_message(
                ChatMessage {
                    role: "assistant".into(),
                    content: msg.content.clone(),
                    tool_calls: msg.tool_calls.clone(),
                    tool_call_id: None,
                    reasoning_content: msg.reasoning_content.clone(),
                },
                asst_tokens,
            );
        }

        // Phase 2 (no tools), or phase 1 with no tool_calls (LLM chose to
        // reply directly) — use content as final reply.
        if !has_tools || msg.tool_calls.as_ref().is_none_or(|c| c.is_empty()) {
            break msg.content;
        }

        // Push the assistant message into the messages vector so the
        // subsequent tool result has a preceding assistant(tool_calls)
        // entry.  Omitting this causes DeepSeek API to return 400:
        // "Messages with role 'tool' must be a response to a preceding
        // message with 'tool_calls'".
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: msg.content.clone(),
            tool_calls: msg.tool_calls.clone(),
            tool_call_id: None,
            reasoning_content: msg.reasoning_content.clone(),
        });

        // Execute tool calls (phase 1).
        let tool_calls = msg.tool_calls.unwrap_or_default();
        for tc in &tool_calls {
            info!(tool = %tc.name, id = %tc.id, "executing feishu tool call");
            let command = tc.arguments["command"].as_str().unwrap_or("").to_owned();
            let al = ctx.allowlist.read().await;

            let (tool_ok, tool_content) = match tools::execute_command(&command, &al, true).await {
                Ok(output) => {
                    let content = if output.stderr.is_empty() {
                        output.stdout
                    } else {
                        format!("exit_code: {:?}\nstdout:\n{}\nstderr:\n{}",
                            output.exit_code, output.stdout, output.stderr)
                    };
                    (true, content)
                }
                Err(e) => (false, e.to_string()),
            };
            drop(al);
            info!(tool = %tc.name, ok = tool_ok, "tool executed");

            let tool_content_clone = tool_content.clone();
            let tool_msg = ChatMessage {
                role: "tool".into(),
                content: tool_content_clone,
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
                reasoning_content: None,
            };
            messages.push(tool_msg);

            // Persist tool result.
            {
                let mut s = session.write().await;
                s.push_message(
                    ChatMessage {
                        role: "tool".into(),
                        content: tool_content.clone(),
                        tool_calls: None,
                        tool_call_id: Some(tc.id.clone()),
                        reasoning_content: None,
                    },
                    provider.count_tokens(&tool_content),
                );
            }
        }

        // Move to phase 2 for next LLM call (no tools, just summarise).
        phase = 2;
    };

    // 6. Send reply via Feishu API.
    let client = match ctx.feishu_client.as_ref() {
        Some(c) => c,
        None => {
            warn!("feishu client not available");
            return;
        }
    };

    // Reply in the same chat where the message came from (group stays group).
    let (reply_id, reply_id_type) = if chat_type == "group" {
        (chat_id.to_owned(), "chat_id")
    } else {
        (open_id.to_owned(), "open_id")
    };

    if let Err(e) = client
        .send_message_to(&reply_id, reply_id_type, json!({"text": reply_text}), "text")
        .await
    {
        error!(err = %e, "failed to send feishu reply");
    }

    // 7. Background memory extraction.
    let mem_mgr = ctx.memory.clone();
    let uid = chat_id.to_owned();
    let umsg = user_text.to_owned();
    let asst = reply_text;
    tokio::spawn(async move {
        if let Err(e) = mem_mgr.extract(&uid, &umsg, &asst).await {
            warn!(err = %e, "feishu memory extraction failed");
        }
    });
}

/// Send a fallback text message when normal processing fails.
async fn send_fallback(ctx: &SharedContext, chat_id: &str, chat_type: &str, open_id: &str, text: &str) {
    let client = match ctx.feishu_client.as_ref() {
        Some(c) => c,
        None => return,
    };
    let (reply_id, reply_id_type) = if chat_type == "group" {
        (chat_id.to_owned(), "chat_id")
    } else {
        (open_id.to_owned(), "open_id")
    };
    let _ = client
        .send_message_to(&reply_id, reply_id_type, json!({"text": text}), "text")
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
