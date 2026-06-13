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
use std::sync::OnceLock;

use arcc_core::context::SharedContext;
use arcc_core::model::types::{ChatMessage, ChatRequest};
use arcc_core::tools;
use arcc_storage::db::models::ScheduledTask;

use arcc_core::feishu_client::FeishuClient;

use super::card;
use super::chat_queue::{ChatEvent, ChatQueue};

/// Singleton queue that serializes processing per chat_id.
fn chat_queue() -> &'static ChatQueue {
    static Q: OnceLock<ChatQueue> = OnceLock::new();
    Q.get_or_init(ChatQueue::new)
}

// ---------------------------------------------------------------------------
// Session history sanitization
// ---------------------------------------------------------------------------

/// Filter a list of ChatMessages so that tool messages are only kept when
/// they are preceded by an assistant message with matching `tool_calls`.
///
/// DeepSeek API requires:
/// 1. Every assistant message carrying `tool_calls` must be immediately
///    followed by tool messages whose `tool_call_id` values match the
///    preceding tool calls.
/// 2. The NUMBER of tool messages must match the NUMBER of tool_calls
///    in the preceding assistant (no gaps, no orphans).
///
/// Orphan tool messages or mismatched counts cause HTTP 400.
fn sanitize_history(history: &mut Vec<ChatMessage>) {
    // Remove leading tool messages (orphans at the start).
    while history.first().is_some_and(|m| m.role == "tool") {
        history.remove(0);
    }
    let mut i = 0;
    while i < history.len() {
        if history[i].role != "assistant" {
            i += 1;
            continue;
        }
        let Some(tool_calls) = history[i].tool_calls.as_ref()
            .filter(|c| !c.is_empty())
        else {
            i += 1;
            continue;
        };
        let expected_count = tool_calls.len();

        // Count how many consecutive tool messages follow this assistant.
        let mut tool_count = 0;
        while i + 1 + tool_count < history.len()
            && history[i + 1 + tool_count].role == "tool"
        {
            tool_count += 1;
        }

        if tool_count >= expected_count {
            // Enough tool messages — skip past the assistant + its tools.
            i += 1 + tool_count;
            continue;
        }

        // Not enough tool messages — strip tool_calls from this assistant
        // so it becomes a plain text message.  Also remove any orphan tools.
        history[i].tool_calls = None;
        for _ in 0..tool_count {
            history.remove(i + 1);
        }
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// Tool handler functions — extracted from the tool-calling loop
// ---------------------------------------------------------------------------

async fn handle_reply_to_user(
    tc: &arcc_core::model::types::ToolCall,
    client: &FeishuClient,
    chat_type: &str,
    open_id: &str,
    reply_id: &str,
    reply_id_type: &str,
) -> (bool, String) {
    let message = tc.arguments["message"].as_str().unwrap_or("");
    let msg_with_mention = maybe_mention(message, chat_type, open_id);
    match client
        .send_message_to(reply_id, reply_id_type, post_md(&msg_with_mention), "post")
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
}

fn handle_get_current_time() -> (bool, String) {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    info!("get_current_time: {now}");
    (true, format!("Current server time: {now}"))
}

async fn handle_schedule_task(
    ctx: &SharedContext,
    tc: &arcc_core::model::types::ToolCall,
    chat_id: &str,
    chat_type: &str,
    open_id: &str,
    reply_id: &str,
    reply_id_type: &str,
) -> (bool, String) {
    let cron_raw = tc.arguments.get("cron").and_then(|v| v.as_str());
    let cron = cron_raw.filter(|s| !s.is_empty());
    if cron_raw.is_some() && cron.is_none() {
        warn!("LLM sent empty cron string, treating as one-shot");
    }
    let delay = tc.arguments.get("delay_seconds").and_then(|v| v.as_i64());
    let task = tc.arguments["task"].as_str().unwrap_or("");

    let (task_cron, next_run_str) =
        if let Some(delay_secs) = delay {
            let fire_at = chrono::Local::now()
                .checked_add_signed(chrono::Duration::seconds(delay_secs))
                .expect("valid duration");
            let formatted = fire_at.format("%Y-%m-%d %H:%M:%S").to_string();
            (None, formatted)
        } else if let Some(cron_expr) = cron {
            let schedule = match cron::Schedule::from_str(cron_expr) {
                Ok(s) => s,
                Err(e) => {
                    warn!(cron_expr, err = %e, "invalid cron expression");
                    return (false, format!("Invalid cron expression '{cron_expr}': {e}"));
                }
            };
            let local = match schedule.upcoming(chrono::Local).next() {
                Some(t) => t,
                None => {
                    warn!(cron_expr, "cron expression never repeats");
                    return (false, "Cron expression never repeats (no future occurrence).".into());
                }
            };
            let formatted = local.format("%Y-%m-%d %H:%M:%S").to_string();
            (Some(cron_expr.to_owned()), formatted)
        } else {
            let formatted = chrono::Local::now()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
            (None, formatted)
        };

    let task_id = uuid::Uuid::new_v4().to_string();
    let scheduled = ScheduledTask {
        id: task_id.clone(),
        chat_id: chat_id.to_owned(),
        chat_type: chat_type.to_owned(),
        open_id: open_id.to_owned(),
        reply_id: reply_id.to_owned(),
        reply_id_type: reply_id_type.to_string(),
        cron: task_cron.clone(),
        task_description: task.to_owned(),
        status: "pending".into(),
        next_run_at: next_run_str.clone(),
        last_run_at: None,
        created_at: None,
        updated_at: None,
    };

    let client = match ctx.feishu_client.as_ref() {
        Some(c) => c,
        None => return (false, "Feishu client not available".into()),
    };

    match ctx.storage.create_scheduled_task(&scheduled) {
        Ok(()) => {
            info!(task_id, next_run = %next_run_str, "task scheduled");
            let cron_display = match &task_cron {
                Some(c) => format!("`{c}`"),
                None => "one-shot".into(),
            };
            let safe_task = task.replace('>', "\\>").replace('\n', " ").replace('\r', "");
            let confirm = format!(
                "✅ Task scheduled!\n> {}\nCron: {}\nNext run: {}",
                safe_task, cron_display, next_run_str,
            );
            let confirm_with_mention = maybe_mention(&confirm, chat_type, open_id);
            let _ = client
                .send_message_to(reply_id, reply_id_type, post_md(&confirm_with_mention), "post")
                .await;
            (true, format!("Task scheduled successfully. Next run at: {next_run_str}"))
        }
        Err(e) => {
            warn!(task_id, err = %e, "failed to persist scheduled task");
            (false, format!("Failed to save task: {e}"))
        }
    }
}

async fn handle_list_tasks(
    ctx: &SharedContext,
    _tc: &arcc_core::model::types::ToolCall,
    chat_id: &str,
) -> (bool, String) {
    match ctx.storage.list_tasks_by_user(chat_id) {
        Ok(tasks) => {
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
        }
        Err(e) => {
            warn!(err = %e, "list_scheduled_tasks failed");
            (false, format!("Failed to list tasks: {e}"))
        }
    }
}

fn handle_use_pro_model(
    _tc: &arcc_core::model::types::ToolCall,
    pro_provider: Option<&std::sync::Arc<dyn arcc_core::model::provider::ModelProvider>>,
    provider: &mut std::sync::Arc<dyn arcc_core::model::provider::ModelProvider>,
) -> (bool, String) {
    if let Some(pro) = pro_provider {
        *provider = pro.clone();
        info!("switched to Pro model for complex task");
        (true, "Switched to Pro model. I now have more reasoning capacity to handle this task.".into())
    } else {
        (false, "Pro model is not available.".into())
    }
}

async fn handle_cancel_task(
    ctx: &SharedContext,
    tc: &arcc_core::model::types::ToolCall,
) -> (bool, String) {
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
}

async fn handle_execute_command(
    ctx: &SharedContext,
    tc: &arcc_core::model::types::ToolCall,
) -> (bool, String) {
    let command = tc.arguments["command"].as_str().unwrap_or("").to_owned();
    let al = ctx.allowlist.read().await;

    match tools::execute_command_with_config(
        &command, &al, true,
        ctx.storage.config.execution.command_timeout_seconds,
        ctx.storage.config.execution.max_output_bytes,
    ).await {
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
    }
}

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

    // Enqueue to per-chat processing pipeline — serializes messages
    // so concurrent writes never corrupt the session VecDeque.
    let ctx = ctx.clone();
    chat_queue().enqueue(ctx, ChatEvent {
        chat_id,
        chat_type,
        open_id,
        message_id,
        user_text,
    }).await;
}

/// Process a Feishu chat message: LLM → tool calls → LLM → send response.
///
/// Returns `true` if the full exchange completed normally, `false` if
/// an error occurred (no provider, no client, LLM call failure, etc.).
/// The scheduler uses this to decide whether to mark a task as completed
/// or retry it on the next tick.
pub(crate) async fn process_feishu_chat(
    ctx: &SharedContext,
    chat_id: &str,
    chat_type: &str,
    open_id: &str,
    _message_id: &str,
    user_text: &str,
) -> bool {
    // 1. Start with Flash (fast, cheap). The AI can call `use_pro_model`
    //    during the tool loop to switch to Pro for complex reasoning.
    let mut provider = match ctx.providers.flash() {
        Some(p) => p.clone(),
        None => {
            warn!("no flash provider available for feishu chat");
            send_fallback(ctx, chat_id, chat_type, open_id, "Service unavailable: no model provider").await;
            return false;
        }
    };

    // The AI can request a switch to Pro mid-conversation via use_pro_model.
    let pro_provider = ctx.providers.pro().cloned();

    // 2. Create/reuse session (keyed by chat_id for continuous conversation).
    let session = ctx.sessions.get_or_create(chat_id, "feishu").await;

    // 3. Save user message to session FIRST so the session VecDeque
    //    maintains chronological order (user → assistant → tool → ...).
    //    The `[Scheduled task trigger]` prefix is used by the scheduler to
    //    help the LLM context but is stripped here for clean session storage.
    // Strip the scheduler's trigger prefix for clean session storage.
    // Format: "[Scheduled task trigger] <task>\nEXECUTE this task NOW..."
    // We keep just the <task> part as the user-visible message.
    let clean_text = if let Some(rest) = user_text.strip_prefix("[Scheduled task trigger] ") {
        rest.lines().next().unwrap_or(rest)
    } else {
        user_text
    };
    let user_tokens = provider.count_tokens(clean_text);
    {
        let mut s = session.write().await;
        s.push_message(
            ChatMessage {
                role: "user".into(),
                content: clean_text.to_owned(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            user_tokens,
        );
    }

    // 4. Build initial messages: system prompt + memory + conversation history.
    //    The current user text is already part of the session (saved above),
    //    so it comes from `context()` as the last message — no separate append needed.
    //    Use the original `user_text` (with prefix if present) so the LLM
    //    receives the full context even though the session stores the clean version.
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
    // of previous turns.  The current user text is already the last
    // message in the session (saved in step 3), so it is included in the
    // history and serves as the prompt for this exchange.
    //
    // Sanitize the history to remove orphan tool messages that would
    // cause DeepSeek API 400 errors ("assistant message with tool_calls
    // must be followed by tool messages").
    let mut history = session.read().await.context();
    sanitize_history(&mut history);
    {
        for msg in history {
            // Skip plain system messages (already added above);
            // but keep summary system messages from compression.
            if msg.role == "system" && !msg.content.starts_with("[conversation summary]") {
                continue;
            }
            messages.push(msg);
        }
    }

    // (user text is already saved in step 3 — not appended here)

    // 5. Get feishu client and compute reply target (needed for ACK + proactive result).
    let client = match ctx.feishu_client.as_ref() {
        Some(c) => c,
        None => {
            warn!("feishu client not available");
            return false;
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
        tools::get_current_time_definition(),
        tools::list_scheduled_tasks_definition(),
        tools::cancel_scheduled_task_definition(),
    ];
    let temperature = ctx.storage.config.model.temperature;
    let max_tokens = ctx.storage.config.model.max_output_tokens;
    let mut phase: u8 = 1;
    let mut tool_rounds: usize = 0;
    const MAX_TOOL_ROUNDS: usize = 10;

    // Track whether `reply_to_user` was called by the AI during the
    // tool loop.  If so, skip the final auto-reply below to avoid
    // sending the user two messages for the same exchange.
    let mut replied_to_user = false;

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
                return false;
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

        // Phase 2 (no tools), or phase 1 with no tool_calls — use content as final reply.
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
            content: display_content.to_owned(),
            tool_calls: msg.tool_calls.clone(),
            tool_call_id: None,
            reasoning_content: msg.reasoning_content.clone(),
        });

        // Execute tool calls (phase 1).
        let tool_calls = msg.tool_calls.unwrap_or_default();
        for tc in &tool_calls {
            info!(tool = %tc.name, id = %tc.id, "executing feishu tool call");

            let (tool_ok, tool_content) = match tc.name.as_str() {
                "reply_to_user" => handle_reply_to_user(tc, client, chat_type, open_id, &reply_id, reply_id_type).await,
                "get_current_time" => handle_get_current_time(),
                "schedule_task" => handle_schedule_task(ctx, tc, chat_id, chat_type, open_id, &reply_id, reply_id_type).await,
                "list_scheduled_tasks" => handle_list_tasks(ctx, tc, chat_id).await,
                "use_pro_model" => handle_use_pro_model(tc, pro_provider.as_ref(), &mut provider),
                "cancel_scheduled_task" => handle_cancel_task(ctx, tc).await,
                _ => handle_execute_command(ctx, tc).await,
            };
            if tc.name == "reply_to_user" && tool_ok {
                replied_to_user = true;
            }
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

        // Stay in Phase 1 so the LLM can orchestrate multi-step tool
        // sequences (e.g. check the time first, then schedule a task).
        // Only move to Phase 2 as a safety stop if the tool rounds exceed
        // the maximum allowed.
        tool_rounds += 1;
        if tool_rounds >= MAX_TOOL_ROUNDS {
            warn!(tool_rounds, "feishu tool calling exceeded max rounds, forcing phase 2");
            phase = 2;
        }
    };

    // 8. Send the final result back to the user.
    // Skip if AI already called `reply_to_user` — otherwise the user
    // gets two messages for the same exchange.
    if !replied_to_user {
        let reply_with_mention = maybe_mention(&reply_text, chat_type, open_id);
        if let Err(e) = client
            .send_message_to(&reply_id, reply_id_type, post_md(&reply_with_mention), "post")
            .await
        {
            warn!(err = %e, "failed to send feishu result");
        }
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

    // 10. Trigger context compression if the session is over its budget.
    if let Some(flash) = ctx.providers.flash() {
        ctx.sessions.compress_all(flash.as_ref()).await;
    }

    true
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
