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

use std::str::FromStr;

use arcc_core::context::SharedContext;
use arcc_core::model::types::{ChatMessage, ChatRequest};
use arcc_core::tools;
use arcc_storage::db::models::ScheduledTask;

use super::card;

/// Build the content body for a Feishu `msg_type: "post"` message.
///
/// The `msg_type: "post"` already indicates this is a rich-text post, so the
/// content JSON string has NO outer `"post"` wrapper — it starts with the
/// language key (`zh_cn`) directly.  Uses `tag: md` for CommonMark/GFM.
///
/// Note: Feishu's `md` tag uses `text` (not `content`) as the field name
/// for the markdown body. Using `content` causes `text field can't be nil`.
///
/// If `text` is empty, falls back to a placeholder.
fn post_md(md_text: &str) -> serde_json::Value {
    let safe_text = if md_text.trim().is_empty() {
        "(no content)"
    } else {
        md_text
    };
    json!({
        "zh_cn": {
            "title": "ARCC",
            "content": [[
                { "tag": "md", "text": safe_text }
            ]]
        }
    })
}

/// In group chats, prepend an @mention so the sender gets notified.
/// The mention uses Feishu's `<at id="open_id">` syntax inside markdown.
fn maybe_mention(text: &str, chat_type: &str, open_id: &str) -> String {
    if chat_type == "group" {
        format!("<at id=\"{open_id}\"></at> {text}")
    } else {
        text.to_owned()
    }
}

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
            handle_message_event(&ctx, &payload.event);
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

/// Check if the LLM's response text promises an action that should have
/// been done via a tool call, but no tool was actually invoked.
///
/// This detects patterns where the AI says things like "已安排" or
/// "已创建定时任务" without actually calling `schedule_task`.  These
/// false promises are the most frequent user-facing issue in server mode.
fn has_unfulfilled_promise(text: &str) -> bool {
    // Explicit "I already did it" commitments (highest confidence).
    let done_commitments = ["已安排", "已创建", "创建了定时", "我已经安排"];
    // Future time guarantees that imply a scheduled action.
    let specific_future = ["一分钟后", "两分钟后"];

    done_commitments.iter().any(|w| text.contains(w))
        || specific_future.iter().any(|w| text.contains(w))
}

/// Handle `im.message.receive_v1` — user sent a message to the bot.
fn handle_message_event(ctx: &SharedContext, event: &serde_json::Value) {
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

    // Strip @mention placeholders (e.g. @_user_1) from the text
    // so the AI only sees the actual user message.
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

    // Spawn background task so the webhook returns 200 immediately.
    let ctx = ctx.clone();
    tokio::spawn(async move {
        process_feishu_chat(&ctx, &chat_id, &chat_type, &open_id, &message_id, &user_text).await;
    });
}

/// Process a Feishu chat message: LLM → tool calls → LLM → send response.
pub(crate) async fn process_feishu_chat(
    ctx: &SharedContext,
    chat_id: &str,
    chat_type: &str,
    open_id: &str,
    _message_id: &str,
    user_text: &str,
) {
    // 1. Start with Flash (fast, cheap). The AI can call `use_pro_model`
    //    during the tool loop to switch to Pro for complex reasoning.
    let mut provider = match ctx.providers.flash() {
        Some(p) => p.clone(),
        None => {
            warn!("no flash provider available for feishu chat");
            send_fallback(ctx, chat_id, chat_type, open_id, "Service unavailable: no model provider").await;
            return;
        }
    };

    // The AI can request a switch to Pro mid-conversation via use_pro_model.
    let pro_provider = ctx.providers.pro().cloned();

    // 2. Create/reuse session (keyed by chat_id for continuous conversation).
    let session = ctx.sessions.get_or_create(chat_id, "feishu").await;

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

    // 4. Build initial messages: system prompt + memory + conversation history + current user text.
    // Memory is keyed by open_id (individual user) even in group chats,
    // so each user's preferences and facts remain private.
    let system_msg = arcc_core::model::prompts::templates::server().to_chat_message();
    let memory_user_id = if chat_type == "group" { open_id } else { chat_id };
    let memory_context = ctx.memory.format_for_context(memory_user_id);

    let mut messages = Vec::new();
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

    // Load conversation history from session so the LLM has full context
    // of previous turns. Without this, every user message is treated as
    // a fresh conversation and the AI "forgets" what was discussed.
    {
        let s = session.read().await;
        let history = s.context();
        for msg in history {
            // Skip plain system messages (already added above);
            // but keep summary system messages from compression.
            if msg.role == "system" && !msg.content.starts_with("[conversation summary]") {
                continue;
            }
            messages.push(msg);
        }
    }

    messages.push(ChatMessage {
        role: "user".into(),
        content: user_text.to_owned(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    // 5. Get feishu client and compute reply target (needed for ACK + proactive result).
    let client = match ctx.feishu_client.as_ref() {
        Some(c) => c,
        None => {
            warn!("feishu client not available");
            return;
        }
    };
    let (reply_id, reply_id_type) = if chat_type == "group" {
        (chat_id.to_owned(), "chat_id")
    } else {
        (open_id.to_owned(), "open_id")
    };

    // 6. Two-phase tool calling loop.
    let tool_defs = vec![
        tools::command_tool_definition(),
        tools::reply_to_user_definition(),
        tools::use_pro_model_definition(),
        tools::schedule_task_definition(),
        tools::list_scheduled_tasks_definition(),
        tools::cancel_scheduled_task_definition(),
    ];
    let temperature = ctx.storage.config.model.temperature;
    let max_tokens = ctx.storage.config.model.max_output_tokens;
    let mut phase = 1;

    let reply_text = loop {
        let has_tools = phase == 1;
        let req = ChatRequest {
            model: provider.model_name().to_owned(),
            messages: messages.clone(),
            tools: if has_tools { Some(tool_defs.clone()) } else { None },
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

        // Ensure content is never empty when tool_calls are present —
        // DeepSeek API requires non-empty content for assistant messages.
        let display_content = if msg.content.trim().is_empty() && msg.tool_calls.is_some() {
            "处理中…"
        } else {
            &msg.content
        };

        // Persist assistant response.
        // CRITICAL: In Phase 2 (has_tools=false), DSML recovery may produce
        // tool_calls from the LLM's markdown output.  These are never executed
        // (the loop breaks below).  Persisting them to the session corrupts the
        // history — the next turn will get "tool_calls must be followed by tool
        // messages" from the API.  Strip them when in Phase 2.
        let tool_calls_for_session = if has_tools { msg.tool_calls.clone() } else { None };
        let asst_tokens = provider.count_tokens(display_content);
        {
            let mut s = session.write().await;
            s.push_message(
                ChatMessage {
                    role: "assistant".into(),
                    content: display_content.to_owned(),
                    tool_calls: tool_calls_for_session,
                    tool_call_id: None,
                    reasoning_content: msg.reasoning_content.clone(),
                },
                asst_tokens,
            );
        }

        // Phase 2 (no tools), or phase 1 with no tool_calls (LLM chose to
        // reply directly) — use content as final reply.
        if !has_tools || msg.tool_calls.as_ref().is_none_or(|c| c.is_empty()) {
            // Phase 1: LLM returned text without calling any tools.
            // Check if it made an unfulfilled promise (said it would
            // schedule/restart/arrange something but didn't actually
            // call the tool).
            if has_tools && has_unfulfilled_promise(&msg.content) {
                warn!("LLM promised action without tool call — forcing retry");
                messages.push(ChatMessage {
                    role: "user".into(),
                    content: "你在回复中承诺了要执行操作，但你没有调用对应的工具来完成。\
                        请立即调用对应的工具来实际执行，不要只文字承诺。".into(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
                continue;
            }
            break msg.content;
        }

        // Push the assistant message into the messages vector so the
        // subsequent tool result has a preceding assistant(tool_calls)
        // entry.  Omitting this causes DeepSeek API to return 400:
        // "Messages with role 'tool' must be a response to a preceding
        // message with 'tool_calls'".
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: display_content.to_owned(),
            tool_calls: msg.tool_calls.clone(),
            tool_call_id: None,
            reasoning_content: msg.reasoning_content.clone(),
        });

        // Execute tool calls (phase 1).
        let tool_calls = msg.tool_calls.unwrap_or_default();
        for tc in &tool_calls {
            info!(tool = %tc.name, id = %tc.id, "executing feishu tool call");

            let (tool_ok, tool_content) = if tc.name == "reply_to_user" {
                let message = tc.arguments["message"].as_str().unwrap_or("");
                let msg_with_mention = maybe_mention(message, chat_type, open_id);
                match client
                    .send_message_to(&reply_id, reply_id_type, post_md(&msg_with_mention), "post")
                    .await
                {
                    Ok(()) => {
                        info!("reply_to_user: message sent to user");
                        (true, "Message sent to user successfully.".into())
                    }
                    Err(e) => {
                        warn!(err = %e, "reply_to_user failed");
                        (false, format!("Failed to send message: {e}"))
                    }
                }
            } else if tc.name == "schedule_task" {
                let cron = tc.arguments["cron"].as_str().unwrap_or("");
                let task = tc.arguments["task"].as_str().unwrap_or("");

                // Validate cron expression.
                let schedule = match cron::Schedule::from_str(cron) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(cron, err = %e, "invalid cron expression");
                        let content = format!("Invalid cron expression '{cron}': {e}");
                        let tool_msg = ChatMessage {
                            role: "tool".into(),
                            content: content.clone(),
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                            reasoning_content: None,
                        };
                        messages.push(tool_msg);
                        continue;
                    }
                };

                let next_run = match schedule.upcoming(chrono::Utc).next() {
                    Some(t) => t,
                    None => {
                        warn!(cron, "cron expression never repeats");
                        let content: String = "Cron expression never repeats (no future occurrence).".into();
                        let tool_msg = ChatMessage {
                            role: "tool".into(),
                            content: content.clone(),
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                            reasoning_content: None,
                        };
                        messages.push(tool_msg);
                        continue;
                    }
                };

                let task_id = uuid::Uuid::new_v4().to_string();
                let scheduled = ScheduledTask {
                    id: task_id.clone(),
                    chat_id: chat_id.to_owned(),
                    chat_type: chat_type.to_owned(),
                    open_id: open_id.to_owned(),
                    reply_id: reply_id.clone(),
                    reply_id_type: reply_id_type.to_string(),
                    cron: Some(cron.to_owned()),
                    task_description: task.to_owned(),
                    status: "pending".into(),
                    next_run_at: next_run.format("%Y-%m-%d %H:%M:%S").to_string(),
                    last_run_at: None,
                    created_at: None,
                    updated_at: None,
                };

                let (tool_ok, tool_content) = match ctx.storage.create_scheduled_task(&scheduled) {
                    Ok(()) => {
                        info!(task_id, next_run = %next_run, "task scheduled");
                        (true, format!(
                            "Task scheduled successfully. Next run at: {}",
                            next_run.format("%Y-%m-%d %H:%M:%S UTC")
                        ))
                    }
                    Err(e) => {
                        warn!(task_id, err = %e, "failed to persist scheduled task");
                        (false, format!("Failed to save task: {e}"))
                    }
                };

                // Notify the user that the task was scheduled.
                if tool_ok {
                    let confirm = format!(
                        "✅ Task scheduled!\n> {}\nCron: `{}`\nNext run: {}",
                        task,
                        cron,
                        next_run.format("%Y-%m-%d %H:%M:%S UTC"),
                    );
                    let confirm_with_mention = maybe_mention(&confirm, chat_type, open_id);
                    let _ = client
                        .send_message_to(&reply_id, reply_id_type, post_md(&confirm_with_mention), "post")
                        .await;
                }

                (tool_ok, tool_content)
            } else if tc.name == "list_scheduled_tasks" {
                let tasks = match ctx.storage.list_tasks_by_user(chat_id) {
                    Ok(t) => t,
                    Err(e) => {
                        warn!(err = %e, "list_scheduled_tasks failed");
                        let content = format!("Failed to list tasks: {e}");
                        let tool_msg = ChatMessage {
                            role: "tool".into(), content: content.clone(),
                            tool_calls: None, tool_call_id: Some(tc.id.clone()),
                            reasoning_content: None,
                        };
                        messages.push(tool_msg);
                        continue;
                    }
                };

                if tasks.is_empty() {
                    (true, "You have no active scheduled tasks.".into())
                } else {
                    let mut lines = String::from("Your scheduled tasks:\n\n");
                    for task in &tasks {
                        lines.push_str(&format!(
                            "- `{}`: {} (cron: {}, next: {}) [{}]\n",
                            task.id, task.task_description,
                            task.cron.as_deref().unwrap_or("one-shot"),
                            task.next_run_at, task.status,
                        ));
                    }
                    (true, lines)
                }
            } else if tc.name == "use_pro_model" {
                if let Some(ref pro) = pro_provider {
                    provider = pro.clone();
                    info!("switched to Pro model for complex task");
                    (true, "Switched to Pro model. I now have more reasoning capacity to handle this task.".into())
                } else {
                    (false, "Pro model is not available.".into())
                }
            } else if tc.name == "cancel_scheduled_task" {
                let task_id = tc.arguments["task_id"].as_str().unwrap_or("");
                let action = tc.arguments["action"].as_str().unwrap_or("delete");

                if action == "pause" {
                    match ctx.storage.pause_task(task_id) {
                        Ok(()) => {
                            info!(task_id, "task paused");
                            (true, format!("Task {task_id} has been paused."))
                        }
                        Err(e) => {
                            warn!(task_id, err = %e, "pause_task failed");
                            (false, format!("Failed to pause task: {e}"))
                        }
                    }
                } else {
                    match ctx.storage.delete_task(task_id) {
                        Ok(true) => {
                            info!(task_id, "task deleted");
                            (true, format!("Task {task_id} has been deleted."))
                        }
                        Ok(false) => (false, format!("Task {task_id} not found.")),
                        Err(e) => {
                            warn!(task_id, err = %e, "delete_task failed");
                            (false, format!("Failed to delete task: {e}"))
                        }
                    }
                }
            } else {
                let command = tc.arguments["command"].as_str().unwrap_or("").to_owned();
                let al = ctx.allowlist.read().await;

                let result = match tools::execute_command(&command, &al, true).await {
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
                result
            };
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

    // 8. Send the final result back to the user.
    let reply_with_mention = maybe_mention(&reply_text, chat_type, open_id);
    if let Err(e) = client
        .send_message_to(&reply_id, reply_id_type, post_md(&reply_with_mention), "post")
        .await
    {
        warn!(err = %e, "failed to send feishu result");
    }

    // 9. Background memory extraction (keyed by open_id for group chats).
    let mem_mgr = ctx.memory.clone();
    let uid = memory_user_id.to_owned();
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
    let text_with_mention = maybe_mention(text, chat_type, open_id);
    let _ = client
        .send_message_to(&reply_id, reply_id_type, post_md(&text_with_mention), "post")
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
