//! Shared IM chat pipeline used by every messaging platform.
//!
//! Platform modules (Feishu, DingTalk, …) own transport concerns: parsing
//! inbound callbacks and formatting outbound messages. This module owns the
//! behavior shared by all of them — per-chat serialization, session history,
//! the two-phase LLM tool loop, scheduling, memory extraction, and
//! compression.
//!
//! Both webhook user messages and scheduler task triggers go through the
//! per-chat queue, so they never race on the same session.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{error, info, warn};

use arcc_core::context::SharedContext;
use arcc_core::model::types::{ChatMessage, ChatRequest, ToolCall};
use arcc_core::tools;
use arcc_storage::db::models::ScheduledTask;

/// Consumer task exits after this long without any new messages.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum number of tool rounds before forcing the final answer phase.
const MAX_TOOL_ROUNDS: usize = 10;

// ---------------------------------------------------------------------------
// Platform abstraction
// ---------------------------------------------------------------------------

/// A platform-neutral inbound chat event.
#[derive(Debug, Clone)]
pub struct ChatEvent {
    /// Origin platform used for sessions, tasks, and backend dispatch.
    pub platform: &'static str,
    /// Session key. DingTalk ids are namespaced (`dingtalk:<id>`).
    pub chat_id: String,
    /// `p2p` or `group`.
    pub chat_type: String,
    /// Sender identity used for @mentions and group memory keys.
    pub user_id: String,
    pub message_id: String,
    /// Persisted outbound target (Feishu open_id/chat_id, DingTalk staffId/conversationId).
    pub reply_id: String,
    /// Type of `reply_id` (`open_id`, `chat_id`, `dingtalk_staff_id`, …).
    pub reply_id_type: String,
    /// DingTalk per-message reply URL; `None` for other platforms.
    pub session_webhook: Option<String>,
    pub user_text: String,
}

/// Transport-specific outbound operations.
#[async_trait]
pub trait ChatBackend: Send + Sync {
    fn platform(&self) -> &'static str;

    /// Send a reply for `event`, adding any platform-specific mention markup.
    async fn send(&self, event: &ChatEvent, text: &str) -> Result<(), String>;

    /// Key under which memories are stored. Group chats attribute memories
    /// to the sender; private chats use the conversation itself.
    fn memory_user_id(&self, event: &ChatEvent) -> String {
        if event.chat_type == "group" {
            event.user_id.clone()
        } else {
            event.chat_id.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Per-chat queue
// ---------------------------------------------------------------------------

enum QueueMsg {
    Event(ChatEvent),
    Scheduler(ChatEvent, oneshot::Sender<bool>),
}

impl QueueMsg {
    fn event(&self) -> &ChatEvent {
        match self {
            QueueMsg::Event(e) | QueueMsg::Scheduler(e, _) => e,
        }
    }

    fn into_event(self) -> (ChatEvent, Option<oneshot::Sender<bool>>) {
        match self {
            QueueMsg::Event(e) => (e, None),
            QueueMsg::Scheduler(e, tx) => (e, Some(tx)),
        }
    }
}

/// A per-chat, single-consumer queue backed by a tokio mpsc channel.
#[derive(Clone)]
pub struct ChatQueue {
    consumers: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<QueueMsg>>>>,
}

impl Default for ChatQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatQueue {
    pub fn new() -> Self {
        Self {
            consumers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Enqueue a user chat event for processing.
    pub async fn enqueue(&self, ctx: SharedContext, event: ChatEvent) {
        self.enqueue_inner(ctx, QueueMsg::Event(event)).await;
    }

    /// Enqueue a scheduler task-trigger event and wait for the result.
    pub async fn enqueue_scheduler(&self, ctx: SharedContext, event: ChatEvent) -> bool {
        let (tx, rx) = oneshot::channel();
        self.enqueue_inner(ctx, QueueMsg::Scheduler(event, tx))
            .await;
        rx.await.unwrap_or(false)
    }

    async fn enqueue_inner(&self, ctx: SharedContext, msg: QueueMsg) {
        let key = queue_key(msg.event());
        let mut map = self.consumers.lock().await;

        if let Some(tx) = map.get(&key) {
            if tx.send(msg).is_err() {
                warn!(key = %key, "chat consumer channel closed unexpectedly");
                map.remove(&key);
            }
            return;
        }

        let (tx, rx) = mpsc::unbounded_channel();
        map.insert(key.clone(), tx);
        drop(map);

        let consumers = self.consumers.clone();
        tokio::spawn(async move {
            info!(key = %key, "chat consumer started");
            Self::consumer_loop(&ctx, &key, rx, msg).await;
            consumers.lock().await.remove(&key);
            info!(key = %key, "chat consumer exited");
        });
    }

    async fn consumer_loop(
        ctx: &SharedContext,
        key: &str,
        rx: mpsc::UnboundedReceiver<QueueMsg>,
        first: QueueMsg,
    ) {
        Self::process_one(ctx, first).await;

        let mut rx = rx;
        loop {
            match tokio::time::timeout(IDLE_TIMEOUT, rx.recv()).await {
                Ok(Some(msg)) => Self::process_one(ctx, msg).await,
                Ok(None) => break,
                Err(_) => {
                    info!(key = %key, idle_timeout = ?IDLE_TIMEOUT, "chat consumer idle, exiting");
                    break;
                }
            }
        }
    }

    async fn process_one(ctx: &SharedContext, msg: QueueMsg) {
        let (event, result_tx) = msg.into_event();

        let ok = match event.platform {
            "feishu" => match ctx.feishu_client.as_ref() {
                Some(client) => {
                    let backend = crate::feishu::backend::FeishuBackend { client };
                    process_chat(ctx, &backend, event).await
                }
                None => {
                    warn!("feishu client not available, dropping chat event");
                    false
                }
            },
            "dingtalk" => match ctx.dingtalk_client.as_ref() {
                Some(client) => {
                    let backend = crate::dingtalk::backend::DingtalkBackend { client };
                    process_chat(ctx, &backend, event).await
                }
                None => {
                    warn!("dingtalk client not available, dropping chat event");
                    false
                }
            },
            other => {
                warn!(platform = other, "unknown chat platform, dropping event");
                false
            }
        };

        if let Some(tx) = result_tx {
            let _ = tx.send(ok);
        }
    }
}

fn queue_key(event: &ChatEvent) -> String {
    format!("{}:{}", event.platform, event.chat_id)
}

/// Process-wide queue shared by webhooks and task executors.
pub fn chat_queue() -> &'static ChatQueue {
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
    while history.first().is_some_and(|m| m.role == "tool") {
        history.remove(0);
    }
    let mut i = 0;
    while i < history.len() {
        if history[i].role != "assistant" {
            i += 1;
            continue;
        }
        let Some(tool_calls) = history[i].tool_calls.as_ref().filter(|c| !c.is_empty()) else {
            i += 1;
            continue;
        };
        let expected_count = tool_calls.len();

        let mut tool_count = 0;
        while i + 1 + tool_count < history.len() && history[i + 1 + tool_count].role == "tool" {
            tool_count += 1;
        }

        if tool_count >= expected_count {
            i += 1 + tool_count;
            continue;
        }

        // Not enough tool messages — strip tool_calls from this assistant
        // so it becomes a plain text message. Also remove any orphan tools.
        history[i].tool_calls = None;
        for _ in 0..tool_count {
            history.remove(i + 1);
        }
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// Tool handlers
// ---------------------------------------------------------------------------

async fn handle_reply_to_user(
    backend: &dyn ChatBackend,
    event: &ChatEvent,
    tc: &ToolCall,
) -> (bool, String) {
    let message = tc.arguments["message"].as_str().unwrap_or("");
    match backend.send(event, message).await {
        Ok(()) => {
            info!(platform = backend.platform(), "reply_to_user: message sent");
            (true, "Message sent to user successfully.".into())
        }
        Err(e) => {
            warn!(platform = backend.platform(), err = %e, "reply_to_user failed");
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
    backend: &dyn ChatBackend,
    event: &ChatEvent,
    tc: &ToolCall,
) -> (bool, String) {
    let cron_raw = tc.arguments.get("cron").and_then(|v| v.as_str());
    let cron = cron_raw.filter(|s| !s.is_empty());
    if cron_raw.is_some() && cron.is_none() {
        warn!("LLM sent empty cron string, treating as one-shot");
    }
    let delay = tc.arguments.get("delay_seconds").and_then(|v| v.as_i64());
    let task = tc.arguments["task"].as_str().unwrap_or("");

    let (task_cron, next_run_str) = if let Some(delay_secs) = delay {
        let fire_at = chrono::Local::now()
            .checked_add_signed(chrono::Duration::seconds(delay_secs))
            .unwrap_or_else(chrono::Local::now);
        let formatted = fire_at.format("%Y-%m-%d %H:%M:%S").to_string();
        (None, formatted)
    } else if let Some(cron_expr) = cron {
        let schedule = match std::str::FromStr::from_str(cron_expr) {
            Ok(s) => s,
            Err(e) => {
                warn!(cron_expr, err = %e, "invalid cron expression");
                return (false, format!("Invalid cron expression '{cron_expr}': {e}"));
            }
        };
        let local = match cron::Schedule::upcoming(&schedule, chrono::Local).next() {
            Some(t) => t,
            None => {
                warn!(cron_expr, "cron expression never repeats");
                return (
                    false,
                    "Cron expression never repeats (no future occurrence).".into(),
                );
            }
        };
        let formatted = local.format("%Y-%m-%d %H:%M:%S").to_string();
        (Some(cron_expr.to_owned()), formatted)
    } else {
        let formatted = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        (None, formatted)
    };

    let task_id = uuid::Uuid::new_v4().to_string();
    let scheduled = ScheduledTask {
        id: task_id.clone(),
        platform: backend.platform().to_owned(),
        chat_id: event.chat_id.clone(),
        chat_type: event.chat_type.clone(),
        open_id: event.user_id.clone(),
        reply_id: event.reply_id.clone(),
        reply_id_type: event.reply_id_type.clone(),
        cron: task_cron.clone(),
        task_description: task.to_owned(),
        status: "pending".into(),
        next_run_at: next_run_str.clone(),
        last_run_at: None,
        created_at: None,
        updated_at: None,
    };

    match ctx.storage.create_scheduled_task(&scheduled) {
        Ok(()) => {
            info!(task_id, platform = backend.platform(), next_run = %next_run_str, "task scheduled");
            let cron_display = match &task_cron {
                Some(c) => format!("`{c}`"),
                None => "one-shot".into(),
            };
            let safe_task = task.replace('\n', " ").replace('\r', "");
            let confirm = format!(
                "✅ Task scheduled!\n> {}\nCron: {}\nNext run: {}",
                safe_task, cron_display, next_run_str,
            );
            let _ = backend.send(event, &confirm).await;
            (
                true,
                format!("Task scheduled successfully. Next run at: {next_run_str}"),
            )
        }
        Err(e) => {
            warn!(task_id, err = %e, "failed to persist scheduled task");
            (false, format!("Failed to save task: {e}"))
        }
    }
}

fn handle_list_tasks(ctx: &SharedContext, chat_id: &str) -> (bool, String) {
    match ctx.storage.list_tasks_by_user(chat_id) {
        Ok(tasks) => {
            if tasks.is_empty() {
                (true, "You have no active scheduled tasks.".into())
            } else {
                let mut lines = String::from("Your scheduled tasks:\n\n");
                for task in &tasks {
                    lines.push_str(&format!(
                        "- `{}`: {} (cron: {}, next: {}) [{}]\n",
                        task.id,
                        task.task_description,
                        task.cron.as_deref().unwrap_or("one-shot"),
                        task.next_run_at,
                        task.status,
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
    pro_provider: Option<&Arc<dyn arcc_core::model::provider::ModelProvider>>,
    provider: &mut Arc<dyn arcc_core::model::provider::ModelProvider>,
) -> (bool, String) {
    if let Some(pro) = pro_provider {
        *provider = pro.clone();
        info!("switched to Pro model for complex task");
        (
            true,
            "Switched to Pro model. I now have more reasoning capacity to handle this task.".into(),
        )
    } else {
        (false, "Pro model is not available.".into())
    }
}

async fn handle_cancel_task(ctx: &SharedContext, tc: &ToolCall) -> (bool, String) {
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

async fn handle_execute_command(ctx: &SharedContext, tc: &ToolCall) -> (bool, String) {
    let command = tc.arguments["command"].as_str().unwrap_or("").to_owned();
    let al = ctx.allowlist.read().await;

    match tools::execute_command_with_config(
        &command,
        &al,
        true,
        ctx.storage.config.execution.command_timeout_seconds,
        ctx.storage.config.execution.max_output_bytes,
    )
    .await
    {
        Ok(output) => (true, output.to_content()),
        Err(e) => (false, e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Main processing pipeline
// ---------------------------------------------------------------------------

/// Process one chat message: LLM → tool calls → LLM → send response.
///
/// Returns `true` if the exchange completed normally, `false` on error
/// (no provider, no backend, LLM failure). The scheduler uses this to
/// decide whether to retry a task on the next tick.
pub async fn process_chat(
    ctx: &SharedContext,
    backend: &dyn ChatBackend,
    event: ChatEvent,
) -> bool {
    let platform = backend.platform();

    // 1. Start with Flash. The AI can switch to Pro via `use_pro_model`.
    let mut provider = match ctx.providers.flash() {
        Some(p) => p.clone(),
        None => {
            warn!(platform, "no flash provider available");
            let _ = backend
                .send(&event, "Service unavailable: no model provider")
                .await;
            return false;
        }
    };
    let pro_provider = ctx.providers.pro().cloned();

    // 2. Create/reuse a session keyed by chat_id.
    let session = ctx.sessions.get_or_create(&event.chat_id, platform).await;

    // 3. Persist the user message first so history stays chronological.
    //    Scheduler triggers carry a prefix for LLM context that is stripped
    //    from the stored user-visible message.
    let clean_text = if let Some(rest) = event.user_text.strip_prefix("[Scheduled task trigger] ") {
        rest.lines().next().unwrap_or(rest)
    } else {
        &event.user_text
    };
    let user_tokens = provider.count_tokens(clean_text);
    {
        let mut s = session.write().await;
        s.push_message(ChatMessage::user(clean_text.to_owned()), user_tokens);
    }

    // 4. Build the prompt: system + memory + conversation history.
    let system_msg = arcc_core::model::prompts::templates::server().to_chat_message();
    let memory_user_id = backend.memory_user_id(&event);
    let memory_context = ctx.memory.format_for_context(&memory_user_id);

    let mut messages = Vec::new();
    messages.push(system_msg);
    if !memory_context.is_empty() {
        messages.push(ChatMessage::system(memory_context));
    }

    let mut history = session.read().await.context();
    sanitize_history(&mut history);
    for msg in history {
        if msg.role == "system" && !msg.content.starts_with("[conversation summary]") {
            continue;
        }
        messages.push(msg);
    }

    // 5. Two-phase tool calling loop.
    let mut tool_defs = vec![
        tools::command_tool_definition(),
        tools::reply_to_user_definition(),
        tools::use_pro_model_definition(),
        tools::schedule_task_definition(),
        tools::get_current_time_definition(),
        tools::list_scheduled_tasks_definition(),
        tools::cancel_scheduled_task_definition(),
    ];
    let server_web_tools = ctx.storage.config.web.server_tools;
    if ctx.storage.config.web.enabled {
        if !server_web_tools {
            tool_defs.push(arcc_core::web::search_tool_definition());
            tool_defs.push(arcc_core::web::fetch_tool_definition());
        }
        tool_defs.push(arcc_core::web::ocr_tool_definition());
    }
    if server_web_tools {
        tool_defs.extend(arcc_core::web::server_tool_definitions());
    }
    let temperature = ctx.storage.config.model.temperature;
    let max_tokens = ctx.storage.config.model.max_output_tokens;
    let mut phase: u8 = 1;
    let mut tool_rounds: usize = 0;
    let mut replied_to_user = false;

    let reply_text = loop {
        let has_tools = phase == 1;
        let req = ChatRequest {
            model: provider.model_name().to_owned(),
            messages: messages.clone(),
            tools: if has_tools {
                let mut defs = tool_defs.clone();
                // Server-side web tools require the flash model; drop them
                // if the AI switched to Pro during this turn.
                if server_web_tools && provider.model_name() != ctx.storage.config.model.flash_model
                {
                    defs.retain(|t| t.builtin.is_none());
                }
                Some(defs)
            } else {
                None
            },
            tool_choice: if has_tools {
                Some(serde_json::json!("auto"))
            } else {
                None
            },
            temperature: Some(temperature),
            max_tokens: Some(max_tokens),
            stream: false,
            thinking_mode: None,
            reasoning_effort: None,
        };

        let response = match provider.chat(req).await {
            Ok(r) => r,
            Err(e) => {
                error!(platform, err = %e, "LLM call failed");
                let _ = backend
                    .send(
                        &event,
                        "Sorry, I encountered an error processing your message.",
                    )
                    .await;
                return false;
            }
        };

        let msg = response.message;

        // DeepSeek requires non-empty content on assistant messages that
        // carry tool_calls.
        let display_content = if msg.content.trim().is_empty() && msg.tool_calls.is_some() {
            "处理中…"
        } else {
            &msg.content
        };

        // Phase 2 may surface DSML-recovered tool_calls; never persist those
        // (they are not executed) or the next turn returns HTTP 400.
        let tool_calls_for_session = if has_tools {
            msg.tool_calls.clone()
        } else {
            None
        };
        let asst_tokens = provider.count_tokens(display_content);
        {
            let mut s = session.write().await;
            s.push_message(
                ChatMessage {
                    role: "assistant".into(),
                    content: display_content.to_owned(),
                    images: Vec::new(),
                    tool_calls: tool_calls_for_session,
                    tool_call_id: None,
                    reasoning_content: msg.reasoning_content.clone(),
                },
                asst_tokens,
            );
        }

        if !has_tools || msg.tool_calls.as_ref().is_none_or(|c| c.is_empty()) {
            break msg.content;
        }

        // The tool results that follow require a preceding assistant
        // message with the matching tool_calls.
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: display_content.to_owned(),
            images: Vec::new(),
            tool_calls: msg.tool_calls.clone(),
            tool_call_id: None,
            reasoning_content: msg.reasoning_content.clone(),
        });

        let tool_calls = msg.tool_calls.unwrap_or_default();
        for tc in &tool_calls {
            info!(platform, tool = %tc.name, id = %tc.id, "executing tool call");

            let (tool_ok, tool_content) = match tc.name.as_str() {
                "reply_to_user" => handle_reply_to_user(backend, &event, tc).await,
                "get_current_time" => handle_get_current_time(),
                "schedule_task" => handle_schedule_task(ctx, backend, &event, tc).await,
                "list_scheduled_tasks" => handle_list_tasks(ctx, &event.chat_id),
                "use_pro_model" => handle_use_pro_model(pro_provider.as_ref(), &mut provider),
                "cancel_scheduled_task" => handle_cancel_task(ctx, tc).await,
                arcc_core::web::SEARCH_TOOL
                | arcc_core::web::FETCH_TOOL
                | arcc_core::web::OCR_TOOL => {
                    let started = Instant::now();
                    let result = arcc_core::web::execute(
                        &ctx.storage.config.model,
                        &ctx.storage.config.web,
                        &tc.name,
                        &tc.arguments,
                    )
                    .await;
                    let elapsed_ms = started.elapsed().as_millis() as u64;
                    arcc_core::web::audit(
                        ctx,
                        &event.chat_id,
                        &tc.name,
                        &tc.arguments,
                        &result,
                        elapsed_ms,
                    );
                    match result {
                        Ok(content) => (true, content),
                        Err(e) => (false, format!("error: {e}")),
                    }
                }
                _ => handle_execute_command(ctx, tc).await,
            };
            if tc.name == "reply_to_user" && tool_ok {
                replied_to_user = true;
            }
            info!(platform, tool = %tc.name, ok = tool_ok, "tool executed");

            messages.push(ChatMessage::tool_result(
                tc.id.clone(),
                tool_content.clone(),
            ));
            {
                let mut s = session.write().await;
                s.push_message(
                    ChatMessage::tool_result(tc.id.clone(), tool_content.clone()),
                    provider.count_tokens(&tool_content),
                );
            }
        }

        tool_rounds += 1;
        if tool_rounds >= MAX_TOOL_ROUNDS {
            warn!(
                platform,
                tool_rounds, "tool calling exceeded max rounds, forcing phase 2"
            );
            phase = 2;
        }
    };

    // 6. Send the final result unless the AI already replied mid-loop.
    if !replied_to_user && let Err(e) = backend.send(&event, &reply_text).await {
        warn!(platform, err = %e, "failed to send final result");
    }

    // 7. Background memory extraction.
    let mem_mgr = ctx.memory.clone();
    let uid = memory_user_id;
    let umsg = event.user_text.clone();
    let asst = reply_text;
    tokio::spawn(async move {
        if let Err(e) = mem_mgr.extract(&uid, &umsg, &asst).await {
            warn!(platform, err = %e, "memory extraction failed");
        }
    });

    // 8. Compress the session if it exceeds its budget.
    if let Some(flash) = ctx.providers.flash() {
        ctx.sessions.compress_all(flash.as_ref()).await;
    }

    true
}
