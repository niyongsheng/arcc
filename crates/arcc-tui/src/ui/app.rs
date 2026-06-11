use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    event::{EnableBracketedPaste, DisableBracketedPaste},
};
use ratatui::layout::Position;
use futures::StreamExt;
use ratatui::Terminal;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, error, info, warn};
use chrono::Utc;
use arcc_storage::audit::types::{Approval, AuditEvent, ExecResult, RiskLevel};

use arcc_core::context::SharedContext;
use arcc_core::model::types::{ChatMessage, ChatRequest, StreamChunk, ToolCall};
use arcc_core::session::Session;
use arcc_core::tools;
use serde_json;
use crate::commands;
use super::components;
use super::logo;
use unicode_width::UnicodeWidthStr;

use crate::event::loop_event::{AppEvent, ConfirmChoice};

/// A command awaiting user permission, with a oneshot channel to resume the
/// tool-execution task.
pub struct CommandConfirm {
    pub command: String,
    pub response_tx: oneshot::Sender<ConfirmChoice>,
}

/// TUI application state.
pub struct App {
    pub input_buffer: String,
    pub character_index: usize,
    pub messages: Vec<String>,
    pub status: String,
    pub tick: u64,
    pub running: bool,
    pub event_rx: mpsc::UnboundedReceiver<AppEvent>,
    /// Sender for AppEvents — shared so background tasks can enqueue events.
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub ctx: SharedContext,

    // ── Background AI task (streaming/thinking) ──
    /// Handle of the currently running AI response task.
    /// Stored so Esc can abort mid-stream.
    pub task_handle: Option<tokio::task::JoinHandle<()>>,

    // ── Multi-turn conversation & thinking mode ──
    pub session: Arc<RwLock<Session>>,
    pub thinking_mode: bool,
    pub reasoning_content: String,

    // ── Chat scroll state ──
    /// Lines to scroll the chat area upward (0 = top/auto-follow).
    pub scroll_offset: usize,

    // ── Interactive permission prompt ──
    /// Non-None when a tool task is waiting for the user to allow/reject a command.
    pub pending_confirm: Option<CommandConfirm>,

    /// Submitted prompts for ↑/↓ navigation.
    input_history: Vec<String>,
    history_index: usize,
    /// Tab completion state.
    completion_candidates: Vec<String>,
    completion_index: usize,
    /// True while a tab completion is active — next tab cycles candidates.
    tab_active: bool,
    /// Blink state — visible when > 0, toggles every 3 Ticks (~1.5 s period).
    blink: u8,

    // ── Dashboard overlay ──
    /// True when the full-screen dashboard replaces the chat area.
    pub show_dashboard: bool,
    /// Pre-collected dashboard data.
    pub dashboard: Option<components::DashboardData>,
    /// Scroll offset for the sessions table within the dashboard.
    pub dashboard_scroll: usize,
    /// Index of the currently selected row in the sessions table.
    pub dashboard_cursor: usize,
    /// Latest live system metrics (CPU, MEM, NET) from background monitor.
    pub live_metrics: components::LiveMetrics,
    /// Background monitor task handle (aborted when dashboard closes).
    pub monitor_handle: Option<tokio::task::JoinHandle<()>>,

    // ── Tree registry for interactive JSON/TOML blocks ──
    pub tree_registry: components::TreeRegistry,
    pub focused_tree: Option<u64>,
}

impl App {
    pub fn new(
        event_tx: mpsc::UnboundedSender<AppEvent>,
        event_rx: mpsc::UnboundedReceiver<AppEvent>,
        ctx: SharedContext,
    ) -> Self {
        // In-memory session — NOT persisted until first user input.
        let context_max = ctx.storage.config.model.context_max_tokens;
        let session = Arc::new(RwLock::new(
            arcc_core::session::Session::new("pending", "tui", "tui", context_max, None),
        ));
        Self {
            input_buffer: String::new(),
            character_index: 0,
            messages: Vec::new(),
            status: "idle".into(),
            tick: 0,
            running: true,
            session,
            thinking_mode: false,
            reasoning_content: String::new(),
            scroll_offset: 0,
            pending_confirm: None,
            input_history: Vec::new(),
            history_index: 0,
            completion_candidates: Vec::new(),
            completion_index: 0,
            tab_active: false,
            blink: 0,
            task_handle: None,
            show_dashboard: false,
            dashboard: None,
            dashboard_scroll: 0,
            dashboard_cursor: 0,
            live_metrics: components::LiveMetrics::default(),
            monitor_handle: None,
            tree_registry: Arc::new(Mutex::new(HashMap::new())),
            focused_tree: None,
            event_tx,
            event_rx,
            ctx,
        }
    }

    fn byte_index(&self) -> usize {
        self.input_buffer
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.character_index)
            .unwrap_or(self.input_buffer.len())
    }

    fn insert_char(&mut self, ch: char) {
        let idx = self.byte_index();
        self.input_buffer.insert(idx, ch);
        self.character_index += 1;
    }

    fn delete_char(&mut self) {
        if self.character_index == 0 {
            return;
        }
        let before = self.input_buffer.chars().take(self.character_index - 1);
        let after = self.input_buffer.chars().skip(self.character_index);
        self.input_buffer = before.chain(after).collect();
        self.character_index -= 1;
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Input(ch) if ch == "\n" || ch == "\r" => {}
            AppEvent::Input(ch) if ch == "\x08" || ch == "\x7f" => {
                if self.status == "idle" {
                    self.delete_char();
                }
            }
            AppEvent::Input(ch) if ch == "\x1b[D" => {
                if self.status == "idle" {
                    self.character_index = self.character_index.saturating_sub(1);
                }
            }
            AppEvent::Input(ch) if ch == "\x1b[C" => {
                if self.status == "idle" {
                    let max = self.input_buffer.chars().count();
                    if self.character_index < max {
                        self.character_index += 1;
                    }
                }
            }
            AppEvent::Input(_ch) if self.status != "idle" => {
                // AI is executing — discard stray keystrokes that may come
                // from subprocesses writing to /dev/tty (e.g. sudo password
                // prompts leaking through the TUI's raw-mode input handler).
            }
            AppEvent::Input(ch) => {
                for c in ch.chars() {
                    self.insert_char(c);
                }
                self.tab_active = false;
            }
            AppEvent::ConfirmCommand { command, tx } => {
                warn!(cmd = %command, "flagged command — awaiting user confirmation");
                self.pending_confirm = Some(CommandConfirm {
                    command: command.clone(),
                    response_tx: tx,
                });
                let sep = "‑".repeat(command.chars().count() + 2).replace('‑', "-");
                self.messages.push(format!("```\n< {command} >\n{sep}\n  \\   ^__^\n   \\  (oo)\\_______\n      (__)\\       )\\/\\\n          ||----w |\n          ||     ||\n```"));
                self.messages.push("**[y]** approve · **[a]** allow always · **[n]** reject".into());
                self.status = "waiting...".into();
            }
            AppEvent::ScrollUp(lines) => {
                if self.show_dashboard {
                    let step = lines as usize;
                    self.dashboard_cursor = self.dashboard_cursor.saturating_sub(step);
                    if self.dashboard_cursor < self.dashboard_scroll {
                        self.dashboard_scroll = self.dashboard_cursor;
                    }
                    return;
                }
                debug!(lines, "scroll up");
                self.scroll_offset = self.scroll_offset.saturating_add(lines as usize);
            }
            AppEvent::ScrollDown(lines) => {
                if self.show_dashboard {
                    let max_rows = self.dashboard.as_ref().map_or(0, |d| d.session_ids.len().saturating_sub(1));
                    let step = lines as usize;
                    self.dashboard_cursor = (self.dashboard_cursor + step).min(max_rows);
                    let body_h = 8;
                    if self.dashboard_cursor >= self.dashboard_scroll + body_h {
                        self.dashboard_scroll = self.dashboard_cursor.saturating_sub(body_h).saturating_add(1);
                    }
                    return;
                }
                debug!("scroll down");
                self.scroll_offset = self.scroll_offset.saturating_sub(lines as usize);
            }
            AppEvent::Token(text) => {
                if self.messages.is_empty() || !self.messages.last().unwrap().starts_with("🤖 ") {
                    self.messages.push("🤖 ".into());
                }
                let last = self.messages.len() - 1;
                self.messages[last].push_str(&text);
                self.status = "streaming".into();
                self.scroll_offset = 0;
            }
            AppEvent::ToolExec(text) => {
                self.messages.push(format!("⚡ {text}"));
                self.scroll_offset = 0;
            }
            AppEvent::StreamDone => {
                self.status = "idle".into();
                self.blink = 0;
                self.reasoning_content.clear();
                if let Some(pending) = self.pending_confirm.take() {
                    let _ = pending.response_tx.send(ConfirmChoice::Reject);
                }
                // Trigger background context compression if session is over threshold.
                let ctx = self.ctx.clone();
                tokio::spawn(async move {
                    if let Some(flash) = ctx.providers.flash() {
                        ctx.sessions.compress_all(&**flash).await;
                    }
                });
            }
            AppEvent::HistoryPrev => {
                // When dashboard is shown, move cursor up in sessions table.
                if self.show_dashboard {
                    if self.dashboard_cursor > 0 {
                        self.dashboard_cursor -= 1;
                        // Auto-scroll: keep cursor in view
                        if self.dashboard_cursor < self.dashboard_scroll {
                            self.dashboard_scroll = self.dashboard_cursor;
                        }
                    }
                    return;
                }
                // Terminal converts mouse scroll-wheel to ↑/↓. When the input
                // buffer is empty, scroll the chat; otherwise navigate history.
                if self.input_buffer.is_empty() {
                    debug!("scroll up via history-prev (empty input)");
                    self.scroll_offset = self.scroll_offset.saturating_add(3);
                    return;
                }
                debug!("history prev");
                if self.history_index < self.input_history.len() {
                    self.history_index += 1;
                    let idx = self.input_history.len() - self.history_index;
                    self.input_buffer = self.input_history[idx].clone();
                    self.character_index = self.input_buffer.chars().count();
                }
            }
            AppEvent::HistoryNext => {
                // When dashboard is shown, move cursor down in sessions table.
                if self.show_dashboard {
                    if let Some(ref data) = self.dashboard
                        && self.dashboard_cursor + 1 < data.session_ids.len()
                    {
                        self.dashboard_cursor += 1;
                        // Auto-scroll: keep cursor in view
                        let body_h = 8;
                        if self.dashboard_cursor >= self.dashboard_scroll + body_h {
                            self.dashboard_scroll = self.dashboard_cursor.saturating_sub(body_h).saturating_add(1);
                        }
                    }
                    return;
                }
                // Terminal converts mouse scroll-wheel to ↑/↓. When the input
                // buffer is empty, scroll the chat; otherwise navigate history.
                if self.input_buffer.is_empty() {
                    debug!("scroll down via history-next (empty input)");
                    self.scroll_offset = self.scroll_offset.saturating_sub(3);
                    return;
                }
                debug!("history next");
                if self.history_index > 0 {
                    self.history_index -= 1;
                    if self.history_index == 0 {
                        self.input_buffer.clear();
                        self.character_index = 0;
                    } else {
                        let idx = self.input_history.len() - self.history_index;
                        self.input_buffer = self.input_history[idx].clone();
                        self.character_index = self.input_buffer.chars().count();
                    }
                }
            }
            AppEvent::Tab => {
                debug!("tab pressed");
                // If input is empty and we have tree blocks, cycle focus.
                if self.input_buffer.is_empty() {
                    let reg = self.tree_registry.lock().unwrap();
                    let hashes: Vec<u64> = reg.keys().copied().collect();
                    if hashes.is_empty() {
                        self.tab_active = false;
                    } else if let Some(current) = self.focused_tree {
                        // Cycle to next hash (wrap around).
                        let pos = hashes.iter().position(|h| *h == current);
                        match pos {
                            Some(p) if p + 1 < hashes.len() => self.focused_tree = Some(hashes[p + 1]),
                            _ => self.focused_tree = Some(hashes[0]),
                        }
                    } else {
                        self.focused_tree = Some(hashes[0]);
                    }
                    return;
                }
                let input = &self.input_buffer;
                if let Some(cmd_prefix) = input.strip_prefix('/') {
                    if !self.tab_active {
                        self.completion_candidates = commands::complete(cmd_prefix).into_iter().map(|s| s.to_string()).collect();
                        self.completion_index = 0;
                        self.tab_active = !self.completion_candidates.is_empty();
                    }
                    if !self.completion_candidates.is_empty() {
                        let name = &self.completion_candidates[self.completion_index];
                        self.input_buffer = format!("/{name} ");
                        self.character_index = self.input_buffer.chars().count();
                        self.completion_index = (self.completion_index + 1) % self.completion_candidates.len();
                    }
                } else {
                    self.tab_active = false;
                }
            }
            AppEvent::Reasoning(text) => {
                self.reasoning_content.push_str(&text);
                if self.messages.last().is_none_or(|m| !m.starts_with("🧠 ")) {
                    self.messages.push("🧠 ".into());
                }
                let last = self.messages.len() - 1;
                self.messages[last].push_str(&text);
                self.status = "thinking".into();
            }
            AppEvent::Tick => {
                self.blink = (self.blink + 1) % 8;
            }
            AppEvent::Resize { cols, rows } => {
                info!(cols, rows, "terminal resize");
            }
            AppEvent::Quit => {
                info!("user quit");
                self.running = false;
            }
            AppEvent::LiveMetrics {
                cpu_pct,
                mem_pct,
                rx_rate,
                tx_rate,
            } => {
                self.live_metrics.cpu_pct = cpu_pct;
                self.live_metrics.mem_pct = mem_pct;
                self.live_metrics.rx_rate = rx_rate;
                self.live_metrics.tx_rate = tx_rate;
            }
            AppEvent::Dismiss => {
                if self.focused_tree.is_some() {
                    self.focused_tree = None;
                } else if self.status != "idle" {
                    // Esc during AI response: abort the streaming task.
                    info!("user aborted AI response");
                    if let Some(handle) = self.task_handle.take() {
                        handle.abort();
                    }
                    self.status = "idle".into();
                    self.messages.push("🤖 _(stopped)_".into());
                    self.blink = 0;
                } else if self.show_dashboard {
                    debug!("dismissing dashboard");
                    self.show_dashboard = false;
                    self.dashboard_scroll = 0;
                    self.dashboard_cursor = 0;
                    stop_monitor(&mut self.monitor_handle);
                } else if self.dashboard.is_some()
                    && self.messages.last().is_some_and(|m| m.contains("Esc to return"))
                {
                    // Re-open dashboard after viewing session messages
                    self.show_dashboard = true;
                    info!("returning to dashboard");
                }
            }
            AppEvent::InteractiveCommand { .. } => {
                // Handled in the main event loop (needs terminal access).
            }
        }
    }

    /// Plan mode — streams a plan-focused prompt through the LLM with thinking mode enabled.
    fn plan_submit(&mut self, task: &str, tx: mpsc::UnboundedSender<AppEvent>) {
        self.ensure_session();
        self.thinking_mode = true;
        self.messages.push(format!("🧑 /plan {task}"));
        self.status = "planning".into();

        let provider = match self.ctx.providers.pick(task.len(), true) {
            Some(p) => p.clone(),
            None => {
                self.messages.push("🤖 No model provider available.".into());
                return;
            }
        };

        // ── Persist plan request to session ──
        {
            let session_id = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    self.session.read().await.id.clone()
                })
            });
            let user_msg = ChatMessage {
                role: "user".into(),
                content: format!("Plan the following task:\n\n{task}"),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            };
            {
                let mut s = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(self.session.write())
                });
                s.push_message(user_msg, provider.count_tokens(task));
            }
            info!(session = %session_id, plan_task_len = task.len(), "plan request submitted");
        }

        let plan_system = arcc_core::model::prompts::templates::plan(task).to_chat_message();

        let ctx = self.ctx.clone();
        let skip_permissions = self.ctx.dangerously_skip_permissions;
        let tool_def = tools::command_tool_definition();
        let temperature = self.ctx.storage.config.model.temperature;
        let max_tokens = self.ctx.storage.config.model.max_output_tokens;
        let session = self.session.clone();
        let plan_system_content = plan_system.content.clone();

        let history_msgs = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { session.read().await.prepare_for_request(true) })
        });
        let initial_messages: Vec<ChatMessage> = std::iter::once(plan_system)
            .chain(history_msgs)
            .collect();

        let handle = tokio::spawn(async move {
            let mut messages = initial_messages;
            let mut phase = 1;

            loop {
                let has_tools = phase == 1;
                let req = ChatRequest {
                    model: provider.model_name().to_owned(),
                    messages: messages.clone(),
                    tools: if has_tools { Some(vec![tool_def.clone()]) } else { None },
                    tool_choice: if has_tools { Some(serde_json::json!("auto")) } else { None },
                    temperature: Some(temperature),
                    max_tokens: Some(max_tokens),
                    stream: true,
                    thinking_mode: None,
                    reasoning_effort: None,
                };

                let mut content_buf = String::new();
                let mut reasoning_buf = String::new();
                let mut tool_calls: Vec<ToolCall> = Vec::new();
                let mut last_usage: Option<arcc_core::model::types::Usage> = None;

                match provider.chat_stream(req).await {
                    Ok(stream) => {
                        let mut stream = Box::pin(stream);
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(StreamChunk::Content(text)) => {
                                    content_buf.push_str(&text);
                                    let _ = tx.send(AppEvent::Token(text));
                                }
                                Ok(StreamChunk::Reasoning(text)) => {
                                    reasoning_buf.push_str(&text);
                                    let _ = tx.send(AppEvent::Reasoning(text));
                                }
                                Ok(StreamChunk::ToolCallStart(tc)) => {
                                    tool_calls.push(tc);
                                }
                                Ok(StreamChunk::Finish(usage)) => {
                                    last_usage = Some(usage);
                                }
                                Ok(StreamChunk::ToolCallEnd { .. }) => {}
                                Err(e) => {
                                    error!(err = %e, "stream error");
                                    let _ = tx.send(AppEvent::Token(format!("\n ❌ stream error: {e}")));
                                    let _ = tx.send(AppEvent::StreamDone);
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::Token(format!("\n ❌ {e}")));
                        let _ = tx.send(AppEvent::StreamDone);
                        return;
                    }
                }

                // Record token usage from API response
                if let Some(ref usage) = last_usage {
                    let sid = session.read().await.id.clone();
                    let mdl = provider.model_name().to_owned();
                    if let Err(e) = ctx.storage.record_token_usage(
                        &sid,
                        &mdl,
                        usage.prompt_tokens as i64,
                        usage.completion_tokens as i64,
                    ) {
                        warn!(err = %e, "failed to record token usage");
                    }
                }

                let has_tool_calls = !tool_calls.is_empty();

                // Persist the assistant response to session.
                {
                    let mut s = session.write().await;
                    let mut assistant_msg = ChatMessage {
                        role: "assistant".into(),
                        content: content_buf.clone(),
                        tool_calls: if has_tool_calls { Some(tool_calls.clone()) } else { None },
                        tool_call_id: None,
                        reasoning_content: if reasoning_buf.is_empty() { None } else { Some(reasoning_buf.clone()) },
                    };
                    let tokens = provider.count_tokens(&content_buf)
                        + provider.count_tokens(&reasoning_buf);
                    s.push_message(assistant_msg.clone(), tokens);
                    assistant_msg.reasoning_content = None;
                }

                if !has_tool_calls {
                    let _ = tx.send(AppEvent::StreamDone);
                    return;
                }

                // Execute tool calls.
                for tc in &tool_calls {
                    info!(tool = %tc.name, id = %tc.id, "executing tool call");
                    let command = tc.arguments["command"].as_str().unwrap_or("").to_owned();
                    let cmd_name = command.split_whitespace().next().unwrap_or(&command).to_string();
                    let _ = tx.send(AppEvent::ToolExec(format!("{command}...")));

                    // Interactive permission check.
                    let confirm_choice = if !skip_permissions {
                        let needs_confirm = ctx.allowlist.read().await.check(&command).unwrap_or(false);
                        if needs_confirm {
                            let (resp_tx, resp_rx) = oneshot::channel();
                            let _ = tx.send(AppEvent::ConfirmCommand {
                                command: command.clone(),
                                tx: resp_tx,
                            });
                            let choice = resp_rx.await.unwrap_or(ConfirmChoice::Reject);
                            if matches!(choice, ConfirmChoice::AllowAlways) {
                                ctx.allowlist.write().await.approve(cmd_name);
                            }
                            choice
                        } else {
                            ConfirmChoice::AllowOnce
                        }
                    } else {
                        ConfirmChoice::AllowOnce
                    };

                    if let ConfirmChoice::Reject = confirm_choice {
                        let mut s = session.write().await;
                        s.push_message(ChatMessage {
                            role: "tool".into(),
                            content: "execution rejected by user".into(),
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                            reasoning_content: None,
                        }, 0);
                        let _ = tx.send(AppEvent::ToolExec("✗ Rejected by user".into()));
                        continue;
                    }

                    // Execute (skip_permissions=true because we already checked).
                    let ai_interactive = tc.arguments.get("interactive")
                        .and_then(|v| v.as_bool());
                    let lower = command.to_lowercase();
                    let words: Vec<&str> = lower.split_whitespace().collect();
                    let first = words.first().copied().unwrap_or("");
                    let auto_interactive = first == "sudo"
                        || words.contains(&"sudo");
                    let interactive = ai_interactive.unwrap_or(auto_interactive);
                    let executed = if interactive {
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        let _ = tx.send(AppEvent::InteractiveCommand {
                            command: command.clone(),
                            response_tx: resp_tx,
                        });
                        let msg = resp_rx.await.unwrap_or_else(|_| "interactive: cancelled".to_string());
                        let exit_code = msg.strip_prefix("exit=")
                            .and_then(|s| s.parse::<i32>().ok());
                        let content = format!("exit_code: {:?}\nstdout:\n(see terminal output above)\n", exit_code);
                        Ok(tools::CommandOutput {
                            stdout: content,
                            stderr: String::new(),
                            exit_code,
                            truncated: false,
                        })
                    } else {
                        let al = ctx.allowlist.read().await;
                        let r = tools::execute_command(&command, &al, true).await;
                        drop(al);
                        r
                    };

                    match executed {
                        Ok(out) => {
                            let content = if out.stderr.is_empty() { out.stdout } else {
                                format!("exit_code: {:?}\nstdout:\n{}\nstderr:\n{}", out.exit_code, out.stdout, out.stderr)
                            };
                            let tokens = provider.count_tokens(&content);
                            let mut s = session.write().await;
                            s.push_message(ChatMessage {
                                role: "tool".into(),
                                content,
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
                                reasoning_content: None,
                            }, tokens);
                            let exec_label = format!("{command} → exit={:?}", out.exit_code);
                            let _ = tx.send(AppEvent::ToolExec(exec_label));
                        }
                        Err(e) => {
                            let mut s = session.write().await;
                            s.push_message(ChatMessage {
                                role: "tool".into(),
                                content: format!("error: {e}"),
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
                                reasoning_content: None,
                            }, 0);
                            let _ = tx.send(AppEvent::ToolExec(format!("{command}: {e}")));
                        }
                    }
                }

                // Rebuild messages for phase 2 from session.
                messages = {
                    let s = session.read().await;
                    let mut base = s.prepare_for_request(true);
                    base.retain(|m| m.role != "system");
                    std::iter::once(ChatMessage {
                        role: "system".into(),
                        content: plan_system_content.clone(),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    })
                        .chain(base)
                        .collect()
                };
                phase = 2;
            }
        });
        self.task_handle = Some(handle);
    }

    /// Ensure a real persisted session exists (lazy creation on first input).
    fn ensure_session(&mut self) {
        if self.session.try_read().ok().map(|s| s.id == "pending").unwrap_or(false) {
            let persisted = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(self.ctx.sessions.create("tui", "tui"))
            });
            self.session = persisted;
            info!("session persisted on first user input");
        }
    }

    /// Submit a prompt and handle the tool-calling stream loop.
    fn submit(&mut self, tx: mpsc::UnboundedSender<AppEvent>) {
        let prompt = std::mem::take(&mut self.input_buffer);
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        // Dismiss dashboard when user types any prompt (except /dashboard itself).
        if self.show_dashboard && prompt != "/dashboard" {
            self.show_dashboard = false;
            self.dashboard_scroll = 0;
            stop_monitor(&mut self.monitor_handle);
        }
        self.input_history.push(prompt.clone());
        self.history_index = 0;

        // Check for slash commands.
        if prompt.starts_with('/') {
            if prompt.starts_with("/plan ") || prompt == "/plan" {
                let task = prompt.strip_prefix("/plan ").unwrap_or("").trim();
                self.plan_submit(task, tx);
                return;
            }
            self.dispatch_command(&prompt);
            return;
        }

        // Persist session only now — proceeding will push messages.
        self.ensure_session();

        self.messages.push(format!("🧑 {prompt}"));
        self.status = "thinking".into();
        self.scroll_offset = 0;

        let provider = match self.ctx.providers.pick(prompt.len(), true) {
            Some(p) => p.clone(),
            None => {
                self.messages.push("🤖 No model provider available.".into());
                self.status = "error".into();
                return;
            }
        };

        // ── Persist user input to session ──
        let session_id = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                self.session.read().await.id.clone()
            })
        });
        let user_msg = ChatMessage {
            role: "user".into(),
            content: prompt.clone(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        {
            let mut s = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(self.session.write())
            });
            s.push_message(user_msg, provider.count_tokens(&prompt));
        }
        info!(session = %session_id, prompt_len = prompt.len(), "user input submitted");

        // ── Audit: user input event ──
        self.ctx.storage.audit.write(&AuditEvent::CommandExec {
            ts: Utc::now().to_rfc3339(),
            session: session_id,
            cmd: prompt.clone(),
            risk: RiskLevel::Low,
            approved_by: Approval::Auto,
            result: ExecResult::Ok,
            elapsed_ms: 0,
        });

        let ctx = self.ctx.clone();
        let skip_permissions = self.ctx.dangerously_skip_permissions;
        let tool_def = tools::command_tool_definition();
        let temperature = self.ctx.storage.config.model.temperature;
        let max_tokens = self.ctx.storage.config.model.max_output_tokens;
        let thinking_mode = self.thinking_mode;
        let session = self.session.clone();

        let system_msg = arcc_core::model::prompts::templates::tui().to_chat_message();

        let history_msgs = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { session.read().await.prepare_for_request(thinking_mode) })
        });
        let initial_messages: Vec<ChatMessage> = std::iter::once(system_msg.clone())
            .chain(history_msgs)
            .collect();

        info!("TUI: starting multi-turn tool-calling stream");

        let handle = tokio::spawn(async move {
            let mut messages = initial_messages;
            let mut phase = 1;

            loop {
                let has_tools = phase == 1;
                let req = ChatRequest {
                    model: provider.model_name().to_owned(),
                    messages: messages.clone(),
                    tools: if has_tools { Some(vec![tool_def.clone()]) } else { None },
                    tool_choice: if has_tools { Some(serde_json::json!("auto")) } else { None },
                    temperature: Some(temperature),
                    max_tokens: Some(max_tokens),
                    stream: true,
                    thinking_mode: if thinking_mode { Some("enabled".into()) } else { None },
                    reasoning_effort: if thinking_mode { Some("max".into()) } else { None },
                };

                let mut content_buf = String::new();
                let mut reasoning_buf = String::new();
                let mut tool_calls: Vec<ToolCall> = Vec::new();
                let mut last_usage: Option<arcc_core::model::types::Usage> = None;

                match provider.chat_stream(req).await {
                    Ok(stream) => {
                        let mut stream = Box::pin(stream);
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(StreamChunk::Content(text)) => {
                                    content_buf.push_str(&text);
                                    let _ = tx.send(AppEvent::Token(text));
                                }
                                Ok(StreamChunk::Reasoning(text)) => {
                                    reasoning_buf.push_str(&text);
                                    let _ = tx.send(AppEvent::Reasoning(text));
                                }
                                Ok(StreamChunk::ToolCallStart(tc)) => {
                                    tool_calls.push(tc);
                                }
                                Ok(StreamChunk::Finish(usage)) => {
                                    last_usage = Some(usage);
                                }
                                Ok(StreamChunk::ToolCallEnd { .. }) => {}
                                Err(e) => {
                                    error!(err = %e, "stream error");
                                    let _ = tx.send(AppEvent::Token(format!("\n ❌ stream error: {e}")));
                                    let _ = tx.send(AppEvent::StreamDone);
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(err = %e, "chat_stream failed");
                        let _ = tx.send(AppEvent::Token(format!("\n ❌ {e}")));
                        let _ = tx.send(AppEvent::StreamDone);
                        return;
                    }
                }

                // Record token usage from API response
                if let Some(ref usage) = last_usage {
                    let sid = session.read().await.id.clone();
                    let mdl = provider.model_name().to_owned();
                    if let Err(e) = ctx.storage.record_token_usage(
                        &sid,
                        &mdl,
                        usage.prompt_tokens as i64,
                        usage.completion_tokens as i64,
                    ) {
                        warn!(err = %e, "failed to record token usage");
                    }
                }

                let has_tool_calls = !tool_calls.is_empty();

                // Persist the assistant response (content + reasoning) to session.
                let content_preview = content_buf.chars().take(120).collect::<String>();
                debug!(content_buf_len = content_buf.len(), has_tool_calls, preview = %content_preview, "persisting assistant response");
                {
                    let mut s = session.write().await;
                    let assistant_msg = ChatMessage {
                        role: "assistant".into(),
                        content: content_buf.clone(),
                        tool_calls: if has_tool_calls { Some(tool_calls.clone()) } else { None },
                        tool_call_id: None,
                        reasoning_content: if reasoning_buf.is_empty() { None } else { Some(reasoning_buf.clone()) },
                    };
                    s.push_message(
                        assistant_msg,
                        provider.count_tokens(&content_buf) + provider.count_tokens(&reasoning_buf),
                    );
                }

                if !has_tool_calls {
                    let _ = tx.send(AppEvent::StreamDone);
                    return;
                }

                // Execute tool calls and persist results.
                for tc in &tool_calls {
                    info!(tool = %tc.name, id = %tc.id, "executing tool call");
                    let command = tc.arguments["command"].as_str().unwrap_or("").to_owned();
                    let cmd_name = command.split_whitespace().next().unwrap_or(&command).to_string();
                    let _ = tx.send(AppEvent::ToolExec(format!("{command}...")));

                    // Interactive permission check.
                    let confirm_choice = if !skip_permissions {
                        let needs_confirm = ctx.allowlist.read().await.check(&command).unwrap_or(false);
                        if needs_confirm {
                            let (resp_tx, resp_rx) = oneshot::channel();
                            let _ = tx.send(AppEvent::ConfirmCommand {
                                command: command.clone(),
                                tx: resp_tx,
                            });
                            let choice = resp_rx.await.unwrap_or(ConfirmChoice::Reject);
                            if matches!(choice, ConfirmChoice::AllowAlways) {
                                ctx.allowlist.write().await.approve(cmd_name);
                            }
                            choice
                        } else {
                            ConfirmChoice::AllowOnce
                        }
                    } else {
                        ConfirmChoice::AllowOnce
                    };

                    if let ConfirmChoice::Reject = confirm_choice {
                        let mut s = session.write().await;
                        s.push_message(ChatMessage {
                            role: "tool".into(),
                            content: "execution rejected by user".into(),
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                            reasoning_content: None,
                        }, 0);
                        let _ = tx.send(AppEvent::ToolExec("✗ Rejected by user".into()));
                        continue;
                    }

                    // Execute (skip_permissions=true because we already checked).
                    let ai_interactive = tc.arguments.get("interactive")
                        .and_then(|v| v.as_bool());
                    let lower = command.to_lowercase();
                    let words: Vec<&str> = lower.split_whitespace().collect();
                    let first = words.first().copied().unwrap_or("");
                    let auto_interactive = first == "sudo"
                        || words.contains(&"sudo");
                    let interactive = ai_interactive.unwrap_or(auto_interactive);
                    let executed = if interactive {
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        let _ = tx.send(AppEvent::InteractiveCommand {
                            command: command.clone(),
                            response_tx: resp_tx,
                        });
                        let msg = resp_rx.await.unwrap_or_else(|_| "interactive: cancelled".to_string());
                        let exit_code = msg.strip_prefix("exit=")
                            .and_then(|s| s.parse::<i32>().ok());
                        let content = format!("exit_code: {:?}\nstdout:\n(see terminal output above)\n", exit_code);
                        Ok(tools::CommandOutput {
                            stdout: content,
                            stderr: String::new(),
                            exit_code,
                            truncated: false,
                        })
                    } else {
                        let al = ctx.allowlist.read().await;
                        let r = tools::execute_command(&command, &al, true).await;
                        drop(al);
                        r
                    };

                    match executed {
                        Ok(output) => {
                            let content = if output.stderr.is_empty() {
                                output.stdout
                            } else {
                                format!("exit_code: {:?}\nstdout:\n{}\nstderr:\n{}",
                                    output.exit_code, output.stdout, output.stderr)
                            };
                            {
                                let mut s = session.write().await;
                                s.push_message(ChatMessage {
                                    role: "tool".into(),
                                    content: content.clone(),
                                    tool_calls: None,
                                    tool_call_id: Some(tc.id.clone()),
                                    reasoning_content: None,
                                }, provider.count_tokens(&content));
                            }
                            let _ = tx.send(AppEvent::ToolExec(format!(
                                "{command} → exit={:?}", output.exit_code)));
                        }
                        Err(e) => {
                            {
                                let mut s = session.write().await;
                                s.push_message(ChatMessage {
                                    role: "tool".into(),
                                    content: format!("error: {e}"),
                                    tool_calls: None,
                                    tool_call_id: Some(tc.id.clone()),
                                    reasoning_content: None,
                                }, 0);
                            }
                            let _ = tx.send(AppEvent::ToolExec(format!("{command}: {e}")));
                        }
                    }
                }

                // Rebuild messages for phase 2 from session (with thinking-mode sanitization).
                messages = {
                    let s = session.read().await;
                    let mut base = s.prepare_for_request(thinking_mode);
                    base.retain(|m| m.role != "system");
                    std::iter::once(system_msg.clone())
                        .chain(base)
                        .collect()
                };
                phase = 2;
            }
        });
        self.task_handle = Some(handle);
    }

    /// Handle a slash command directly (no LLM).
    fn dispatch_command(&mut self, input: &str) {
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.is_empty() {
            return;
        }
        let cmd_name = parts[0].strip_prefix('/').unwrap_or(parts[0]);

        match cmd_name {
            "help" => {
                let sub = parts.get(1).copied();
                if let Some(name) = sub {
                    if let Some(c) = commands::find(name) {
                        self.messages.push(format!("🤖 **{}**  — {}", c.usage, c.desc));
                        self.messages.push(format!("🤖 Category: **{}**", c.cat.label()));
                    } else {
                        self.messages.push(format!("🤖 Unknown command: `/{name}`"));
                    }
                } else {
                    for line in commands::help_all() {
                        self.messages.push(line);
                    }
                }
            }
            "clear" => {
                info!("session cleared");
                if let Some(pending) = self.pending_confirm.take() {
                    let _ = pending.response_tx.send(ConfirmChoice::Reject);
                }
                self.messages.clear();
                self.reasoning_content.clear();
                // Only create a new DB session if we had a real one before
                let is_pending = self.session.try_read().ok().map(|s| s.id == "pending").unwrap_or(true);
                if is_pending {
                    // Reset to a fresh in-memory session
                    let ctx = &self.ctx;
                    let context_max = ctx.storage.config.model.context_max_tokens;
                    self.session = Arc::new(RwLock::new(
                        arcc_core::session::Session::new("pending", "tui", "tui", context_max, None),
                    ));
                } else {
                    let new_session = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current()
                            .block_on(self.ctx.sessions.create("tui", "tui"))
                    });
                    self.session = new_session;
                }
            }
            "model" => {
                let flash = self.ctx.providers.flash().map(|p| p.model_name()).unwrap_or("?");
                let pro = self.ctx.providers.pro().map(|p| p.model_name()).unwrap_or("?");
                info!(flash, pro, "model info");
                self.messages.push(format!("🤖 flash model: {flash}"));
                self.messages.push(format!("🤖 pro model:   {pro}"));
            }
            "skills" => {
                self.messages.push("🤖 MCP tools:".into());
                let tools = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(self.ctx.mcp.list_tools())
                });
                if tools.is_empty() {
                    self.messages.push("🤖   (none registered — use /exec to run commands directly)".into());
                } else {
                    for t in &tools {
                        self.messages.push(format!("🤖   - **{}**: {}", t.name, t.description));
                    }
                }
            }
            "exec" => {
                let command = parts[1..].join(" ");
                if command.is_empty() {
                    self.messages.push("🤖 Usage: `/exec <command>`".into());
                    return;
                }
                info!(%command, "exec slash command");

                // Safety check via allowlist.
                let allowlist = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(self.ctx.allowlist.read())
                });
                let blocked = allowlist.check(&command).unwrap_or(false);
                drop(allowlist);

                if blocked {
                    self.messages.push(format!("⚠ Command blocked by allowlist: `{command}`"));
                    return;
                }

                // Audit
                self.ctx.storage.audit.write(&AuditEvent::CommandExec {
                    ts: Utc::now().to_rfc3339(),
                    session: "...".into(),
                    cmd: command.clone(),
                    risk: RiskLevel::Low,
                    approved_by: Approval::Auto,
                    result: ExecResult::Ok,
                    elapsed_ms: 0,
                });

                // Execute command.
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let al = self.ctx.allowlist.read().await;
                        tools::execute_command(&command, &al, true).await
                    })
                });

                match result {
                    Ok(out) => {
                        let content = if out.stderr.is_empty() { out.stdout } else {
                            format!("exit_code: {:?}\nstdout:\n{}\nstderr:\n{}", out.exit_code, out.stdout, out.stderr)
                        };
                        self.messages.push(format!("⚡ exit={:?}\n```\n{content}\n```", out.exit_code));
                    }
                    Err(e) => {
                        self.messages.push(format!("⚡ {e}"));
                    }
                }
            }
            "thinking" => {
                self.thinking_mode = !self.thinking_mode;
                let state = if self.thinking_mode { "on" } else { "off" };
                info!(thinking = state, "thinking mode toggled");
                self.messages.push(format!("🤖 Thinking mode: **{state}**"));
            }
            "stats" => {
                let msg_count = self.messages.len();
                let history_count = self.input_history.len();
                let session_id = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        self.session.read().await.id.clone()
                    })
                });
                info!(msg_count, history_count, %session_id, "stats");
                self.messages.push(format!("🤖 Messages: {msg_count}"));
                self.messages.push(format!("🤖 History: {history_count} entries"));
                self.messages.push(format!("🤖 Session: `{session_id}`"));
            }
            "dashboard" | "data" => {
                self.show_dashboard = !self.show_dashboard;
                if self.show_dashboard {
                    self.dashboard = Some(collect_dashboard_data(&self.ctx));
                    self.dashboard_scroll = 0;
                    self.dashboard_cursor = 0;
                    info!("dashboard opened");
                    start_monitor(&mut self.monitor_handle, self.event_tx.clone());
                } else {
                    stop_monitor(&mut self.monitor_handle);
                }
            }
            "exit" | "quit" => {
                info!("user quit via command");
                self.running = false;
            }
            _ => {
                self.messages.push(format!("🤖 Unknown command: `{input}` — try `/help`"));
            }
        }
    }
}

// ── Dashboard data collection ───────────────────────────────────────

/// Collect all data needed to render the dashboard overlay.
fn collect_dashboard_data(ctx: &arcc_core::context::SharedContext) -> components::DashboardData {
    // System info
    let hostname = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "?".into());
    let os = std::env::consts::OS.to_owned();
    let arch = std::env::consts::ARCH.to_owned();
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let uptime = get_uptime();
    let (mem_total, mem_used) = get_memory_info();

    let total_tokens = ctx.storage.total_tokens(7).unwrap_or((0, 0));
    let (total_input, total_output) = total_tokens;

    // Sessions
    let sessions = ctx.storage.list_sessions(50).unwrap_or_default();
    let session_count = sessions.len();
    let total_msgs: usize = ctx.storage.message_count().unwrap_or(0) as usize;

    // Format session rows for table display
    let session_rows: Vec<String> = sessions.iter().map(|s| {
        let id_short = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
        let active = fmt_local_ts(&s.last_active_at);
        format!("{}|{}|{}|{}", id_short, s.name, s.mode, active)
    }).collect();
    let session_ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();

    // Daily token usage
    let token_daily = ctx.storage.token_usage_daily(7).unwrap_or_default();
    let chart_data: Vec<(String, u64)> = token_daily.iter().map(|r| {
        let label = if r.date.len() >= 10 {
            r.date[8..10].trim_start_matches('0').to_owned() // "11"
        } else {
            r.date.clone()
        };
        let total = (r.input_tokens + r.output_tokens) as u64;
        (label, total)
    }).collect();

    // Recent audit events
    let audit = ctx.storage.recent_audit(10).unwrap_or_default();
    let audit_items: Vec<(String, String, bool)> = audit.iter().map(|ev| {
        match ev {
            arcc_storage::audit::types::AuditEvent::CommandExec { ts, cmd, result, .. } => {
                let ok = matches!(result, arcc_storage::audit::types::ExecResult::Ok);
                let ts = fmt_local_ts(ts);
                (ts, format!("cmd {cmd}"), ok)
            }
            arcc_storage::audit::types::AuditEvent::CommandBlocked { ts, cmd, .. } => {
                let ts = fmt_local_ts(ts);
                (ts, format!("blocked {cmd}"), false)
            }
            arcc_storage::audit::types::AuditEvent::McpToolCall { ts, tool, result, .. } => {
                let ok = matches!(result, arcc_storage::audit::types::ExecResult::Ok);
                let ts = fmt_local_ts(ts);
                (ts, format!("mcp {tool}"), ok)
            }
            arcc_storage::audit::types::AuditEvent::HumanConfirm { ts, action, decision, .. } => {
                let ok = matches!(decision, arcc_storage::audit::types::ConfirmDecision::Approved);
                let ts = fmt_local_ts(ts);
                (ts, format!("confirm {action}"), ok)
            }
        }
    }).collect();

    components::DashboardData {
        system: components::SystemInfo {
            hostname,
            os: format!("{os} ({arch})"),
            cpu_count,
            uptime: uptime.unwrap_or_else(|| "?".into()),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            memory_total_mb: mem_total,
            memory_used_mb: mem_used,
        },
        sessions: session_rows,
        session_ids,
        session_count,
        msg_count: total_msgs,
        token_daily: chart_data,
        total_input,
        total_output,
        audit_items,
    }
}

/// Get a human-readable uptime string (platform-specific).
fn get_uptime() -> Option<String> {
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .ok()?;
        let s = String::from_utf8(out.stdout).ok()?;
        // Parse: { sec = 1234567890, usec = 0 } Tue Jun 11 ...
        let sec = s.split("sec = ").nth(1)?.split(',').next()?;
        let boot_secs: u64 = sec.trim().parse().ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        let up = now - boot_secs;
        let d = up / 86400;
        let h = (up % 86400) / 3600;
        let m = (up % 3600) / 60;
        Some(if d > 0 {
            format!("{d}d {h}h {m}m")
        } else if h > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{m}m")
        })
    } else if cfg!(target_os = "linux") {
        let content = std::fs::read_to_string("/proc/uptime").ok()?;
        let secs: f64 = content.split_whitespace().next()?.parse().ok()?;
        let up = secs as u64;
        let d = up / 86400;
        let h = (up % 86400) / 3600;
        let m = (up % 3600) / 60;
        Some(if d > 0 {
            format!("{d}d {h}h {m}m")
        } else if h > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{m}m")
        })
    } else {
        None
    }
}

/// Get memory info: (total_mb, used_mb). Returns (0,0) on failure.
fn get_memory_info() -> (u64, u64) {
    if cfg!(target_os = "macos") {
        let total = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok());

        let vm = std::process::Command::new("vm_stat")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok());

        if let (Some(total_bytes), Some(ref vm_out)) = (total, vm) {
            let page_size = vm_out
                .lines()
                .next()
                .and_then(|l| l.split("page size of ").nth(1))
                .and_then(|s| s.split(" bytes").next())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(16384);

            let active = parse_vm_val(vm_out, "Pages active:");
            let wired = parse_vm_val(vm_out, "Pages wired down:");
            let compressed = parse_vm_val(vm_out, "Pages occupied by compressor:");

            let used_bytes = (active + wired + compressed) * page_size;
            return (total_bytes / 1_048_576, used_bytes / 1_048_576);
        }
    } else if cfg!(target_os = "linux")
        && let Ok(content) = std::fs::read_to_string("/proc/meminfo")
    {
        let total_kb = parse_proc_val(&content, "MemTotal:");
        let avail_kb = parse_proc_val(&content, "MemAvailable:");
        if let (Some(t), Some(a)) = (total_kb, avail_kb) {
            return (t / 1024, (t - a) / 1024);
        }
    }
    (0, 0)
}

fn parse_vm_val(output: &str, key: &str) -> u64 {
    output
        .lines()
        .find(|l| l.contains(key))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|s| s.trim().trim_end_matches('.').parse::<u64>().ok())
        .unwrap_or(0)
}

/// Parse an RFC 3339 or SQLite timestamp and format as local `YYYY-MM-DD HH:MM:SS`.
fn fmt_local_ts(ts: &str) -> String {
    // RFC 3339 with timezone (e.g. "2026-06-11T01:26:37.630464Z")
    if let Ok(dt) = ts.parse::<chrono::DateTime<chrono::Utc>>() {
        return dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string();
    }
    // SQLite format (no timezone — interpret as UTC)
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S") {
        let utc: chrono::DateTime<chrono::Utc> =
            chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc);
        return utc.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string();
    }
    // Fallback: return as-is (strip T if present)
    ts.replace('T', " ")
}

fn parse_proc_val(content: &str, key: &str) -> Option<u64> {
    content
        .lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u64>().ok())
}

// ── Live system monitor ─────────────────────────────────────────────

/// Stop the background monitor task if running.
fn stop_monitor(handle: &mut Option<tokio::task::JoinHandle<()>>) {
    if let Some(h) = handle.take() {
        h.abort();
    }
}

/// Start a background task that collects CPU/memory/network every 2 s.
fn start_monitor(
    handle: &mut Option<tokio::task::JoinHandle<()>>,
    tx: mpsc::UnboundedSender<crate::event::loop_event::AppEvent>,
) {
    stop_monitor(handle);
    let core_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    *handle = Some(tokio::spawn(async move {
        let mut prev_rx = 0u64;
        let mut prev_tx = 0u64;
        let mut first = true;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        // Small initial delay so first render gets data promptly
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        loop {
            interval.tick().await;

            // ── CPU (total % across all processes, normalized to 0-100) ──
            let (cpu_pct, mem_pct, rx_bytes, tx_bytes) =
                tokio::task::spawn_blocking(move || {
                    let c = get_live_cpu(core_count);
                    let m = get_live_mem();
                    let (rx, tx) = get_live_net();
                    (c, m, rx, tx)
                })
                .await
                .unwrap_or((0.0, 0.0, 0, 0));
            let (rx_rate, tx_rate) = if first {
                first = false;
                prev_rx = rx_bytes;
                prev_tx = tx_bytes;
                (0.0, 0.0)
            } else {
                let dt = 2.0f64; // 2 s interval
                let r = rx_bytes.saturating_sub(prev_rx) as f64 / dt;
                let t = tx_bytes.saturating_sub(prev_tx) as f64 / dt;
                prev_rx = rx_bytes;
                prev_tx = tx_bytes;
                (r, t)
            };

            let _ = tx.send(crate::event::loop_event::AppEvent::LiveMetrics {
                cpu_pct,
                mem_pct,
                rx_rate,
                tx_rate,
            });
        }
    }));
}

/// Get system-wide CPU usage as a percentage (0.0 – 100.0).
fn get_live_cpu(core_count: usize) -> f64 {
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("ps")
            .args(["-A", "-o", "%cpu="])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok());
        if let Some(output) = out {
            let total: f64 = output.lines().filter_map(|l| {
                let s = l.trim().replace(',', ".");
                s.parse::<f64>().ok()
            }).sum();
            return (total / core_count as f64).clamp(0.0, 100.0);
        }
    } else if cfg!(target_os = "linux") {
        // Read /proc/stat for CPU ticks
        if let Ok(content) = std::fs::read_to_string("/proc/stat") {
            let line = content.lines().next().unwrap_or("");
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                let user: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let nice: u64 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                let system: u64 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
                let idle: u64 = parts.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
                let total = user + nice + system + idle;
                if total > 0 {
                    return (user + nice + system) as f64 / total as f64 * 100.0;
                }
            }
        }
    }
    0.0
}

/// Get memory usage as a percentage (0.0 – 100.0).
fn get_live_mem() -> f64 {
    if cfg!(target_os = "macos") {
        let total = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok());
        let vm = std::process::Command::new("vm_stat")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok());
        if let (Some(t), Some(ref v)) = (total, vm) {
            let page_size = v
                .lines().next()
                .and_then(|l| l.split("page size of ").nth(1))
                .and_then(|s| s.split(" bytes").next())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(16384);
            let active = parse_vm_val(v, "Pages active:");
            let wired = parse_vm_val(v, "Pages wired down:");
            let compressed = parse_vm_val(v, "Pages occupied by compressor:");
            let used = (active + wired + compressed) * page_size;
            return (used as f64 / t as f64 * 100.0).clamp(0.0, 100.0);
        }
    } else if cfg!(target_os = "linux")
        && let Ok(content) = std::fs::read_to_string("/proc/meminfo")
    {
        let total_kb = parse_proc_val(&content, "MemTotal:");
        let avail_kb = parse_proc_val(&content, "MemAvailable:");
        if let (Some(t), Some(a)) = (total_kb, avail_kb) {
            return (t - a) as f64 / t as f64 * 100.0;
        }
    }
    0.0
}

/// Get cumulative network bytes (rx, tx) for the first active non-lo interface.
/// Finds column positions dynamically from the `netstat -ib` header line.
fn get_live_net() -> (u64, u64) {
    let output = std::process::Command::new("netstat")
        .args(["-ib", "-n"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let mut lines = output.lines();
    // First line: header — find column indices by name
    let header = match lines.next() {
        Some(h) => h,
        None => return (0, 0),
    };
    let hcols: Vec<&str> = header.split_whitespace().collect();
    let ib_idx = hcols.iter().position(|&c| c.contains("Ibytes") || c.contains("ibytes"));
    let ob_idx = hcols.iter().position(|&c| c.contains("Obytes") || c.contains("obytes"));
    let (ib_idx, ob_idx) = match (ib_idx, ob_idx) {
        (Some(i), Some(o)) => (i, o),
        _ => return (0, 0),
    };
    for line in lines {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() <= ib_idx.max(ob_idx) {
            continue;
        }
        let name = cols.first().copied().unwrap_or("");
        if name.starts_with("lo") || name.is_empty() {
            continue;
        }
        let ib = cols.get(ib_idx).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let ob = cols.get(ob_idx).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        if ib > 0 || ob > 0 {
            return (ib, ob);
        }
    }
    (0, 0)
}

// ---------------------------------------------------------------------------
// Main TUI loop
// ---------------------------------------------------------------------------

/// Run the TUI main loop — Claude Code CLI style.
pub async fn run(ctx: SharedContext) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    execute!(stdout, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    let (tx, rx) = crate::event::loop_event::create_event_loop();
    let mut input_handle = crate::event::handler::spawn_input_handler(tx.clone());
    let mut app = App::new(tx.clone(), rx, ctx);

    while app.running {
        while let Ok(event) = app.event_rx.try_recv() {
            match event {
                AppEvent::Input(ch) if ch == "\n" || ch == "\r" => {
                    // If a tree block is focused and input is empty, cycle its view mode.
                    if app.input_buffer.trim().is_empty() && app.focused_tree.is_some() {
                        let hash = app.focused_tree.unwrap();
                        let mut reg = app.tree_registry.lock().unwrap();
                        if let Some(entry) = reg.get_mut(&hash) {
                            entry.mode = match entry.mode {
                                components::TreeViewMode::Collapsed => components::TreeViewMode::Expanded,
                                components::TreeViewMode::Expanded => components::TreeViewMode::Raw,
                                components::TreeViewMode::Raw => components::TreeViewMode::Collapsed,
                            };
                        }
                        app.input_buffer.clear();
                        app.character_index = 0;
                        continue;
                    }
                    // Check for pending permission prompt first.
                    if let Some(pending) = app.pending_confirm.take() {
                        let input = app.input_buffer.trim().to_lowercase();
                        let cmd_name = pending.command.split_whitespace().next()
                            .unwrap_or(&pending.command).to_string();
                        let session_id = tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(async {
                                app.session.read().await.id.clone()
                            })
                        });
                        match input.as_str() {
                            "a" | "always" => {
                                let mut al = tokio::task::block_in_place(|| {
                                    tokio::runtime::Handle::current()
                                        .block_on(app.ctx.allowlist.write())
                                });
                                al.approve(cmd_name);
                                drop(al);
                                app.messages.push("✓ **Moo!** — added to allowlist, won't ask again".into());
                                let _ = pending.response_tx.send(ConfirmChoice::AllowAlways);
                                app.ctx.storage.audit.write(&AuditEvent::HumanConfirm {
                                    ts: Utc::now().to_rfc3339(),
                                    session: session_id,
                                    action: format!("exec: {}", pending.command),
                                    decision: arcc_storage::audit::types::ConfirmDecision::Approved,
                                    user: "tui_user".into(),
                                });
                            }
                            "y" | "yes" => {
                                app.messages.push("✓ Allowed once".into());
                                let _ = pending.response_tx.send(ConfirmChoice::AllowOnce);
                                app.ctx.storage.audit.write(&AuditEvent::HumanConfirm {
                                    ts: Utc::now().to_rfc3339(),
                                    session: session_id,
                                    action: format!("exec: {}", pending.command),
                                    decision: arcc_storage::audit::types::ConfirmDecision::Approved,
                                    user: "tui_user".into(),
                                });
                            }
                            _ => {
                                app.messages.push("✗ Rejected".into());
                                let _ = pending.response_tx.send(ConfirmChoice::Reject);
                                app.ctx.storage.audit.write(&AuditEvent::HumanConfirm {
                                    ts: Utc::now().to_rfc3339(),
                                    session: session_id,
                                    action: format!("exec: {}", pending.command),
                                    decision: arcc_storage::audit::types::ConfirmDecision::Denied,
                                    user: "tui_user".into(),
                                });
                            }
                        }
                        app.input_buffer.clear();
                        app.character_index = 0;
                        app.status = "idle".into();
                    } else if app.show_dashboard && app.input_buffer.trim().is_empty() {
                        // Enter with empty input while dashboard is shown:
                        // show messages for the selected session, then close dashboard.
                        if let Some(ref data) = app.dashboard.clone() {
                            let idx = app.dashboard_cursor;
                            if idx < data.session_ids.len() {
                                let session_id = &data.session_ids[idx];
                                let short_id = if session_id.len() > 8 {
                                    &session_id[..8]
                                } else {
                                    session_id.as_str()
                                };
                                app.show_dashboard = false;
                                // Keep dashboard data cached so Esc can re-open.
                                app.dashboard_cursor = 0;
                                app.dashboard_scroll = 0;

                                // Fetch session messages
                                let msgs = app.ctx.storage.session_messages(session_id, 20).ok();
                                if let Some(msgs) = msgs {
                                    app.messages.clear();
                                    app.messages
                                        .push(format!("🤖 **Session `{short_id}` — last {} messages**", msgs.len()));
                                    for m in msgs.iter().rev() {
                                        let role_emoji = match m.role.as_str() {
                                            "user" => "🧑",
                                            "assistant" => "🤖",
                                            "tool" => "⚡",
                                            _ => "📝",
                                        };
                                        let raw_preview: String = m.content.chars().take(160).collect();
                                        let ellipsis = if raw_preview.len() < m.content.len() {
                                            "…"
                                        } else {
                                            ""
                                        };
                                        let one_line = raw_preview
                                            .replace('\n', " ")
                                            .replace('\r', "");
                                        let tokens = m
                                            .token_count
                                            .map(|t| format!(" ({} tok)", t))
                                            .unwrap_or_default();
                                        let ts = m.created_at.as_deref().unwrap_or("?");
                                        app.messages.push(format!(
                                            "{} {} · {}{}{}",
                                            role_emoji, one_line, ts, tokens, ellipsis,
                                        ));
                                    }
                                    app.messages
                                        .push("🤖 Press Esc to return to dashboard".into());
                                } else {
                                    app.messages.push("⚠ Failed to load session messages".into());
                                }
                                app.input_buffer.clear();
                                app.character_index = 0;
                            }
                        }
                    } else {
                        app.submit(tx.clone());
                    }
                }
                AppEvent::InteractiveCommand { command, response_tx } => {
                    // Pause the crossterm input handler so it doesn't compete
                    // with the child process for stdin.
                    input_handle.abort();
                    let _ = input_handle.await;

                    // Exit alternate screen + raw mode → child output goes to
                    // the primary screen, cleanly separated from the TUI.
                    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                    disable_raw_mode()?;

                    let shell = if cfg!(target_os = "windows") { "cmd.exe" } else { "sh" };
                    let arg = if cfg!(target_os = "windows") { "/C" } else { "-c" };
                    let exit_code = std::process::Command::new(shell)
                        .arg(arg)
                        .arg(&command)
                        .stdin(std::process::Stdio::inherit())
                        .stdout(std::process::Stdio::inherit())
                        .stderr(std::process::Stdio::inherit())
                        .spawn()
                        .and_then(|mut child| child.wait())
                        .map(|status| status.code().unwrap_or(-1))
                        .unwrap_or(-1);

                    // Re-enter TUI mode.
                    enable_raw_mode()?;
                    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                    terminal.clear()?;

                    // Re-spawn the input handler now that the child is done.
                    input_handle = crate::event::handler::spawn_input_handler(tx.clone());
                    let _ = response_tx.send(format!("exit={exit_code}"));
                }
                AppEvent::Quit => {
                    app.running = false;
                    break;
                }
                other => app.handle_event(other),
            }
        }

        // Read session info before draw (avoid tokio RwLock inside sync closure).
        let session_info = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let s = app.session.read().await;
                (s.id.clone(), s.mode.clone())
            })
        });

        terminal.draw(|f| {
            let areas = components::main_layout(f.area());

            let phase = if app.status != "idle" { app.blink } else { 99 };
            components::render_title(f, areas[0], &session_info.0, &session_info.1, phase);

            components::render_status(f, areas[2], &app.status, app.tick, app.thinking_mode);
            if app.show_dashboard {
                if let Some(ref data) = app.dashboard {
                    components::render_dashboard(
                        f,
                        areas[1],
                        data,
                        app.dashboard_scroll,
                        app.dashboard_cursor,
                        &app.live_metrics,
                    );
                } else {
                    components::render_chat(
    f,
    areas[1],
    &app.messages,
    app.scroll_offset,
    &app.tree_registry,
    app.focused_tree,
);
                }
            } else if app.messages.is_empty() {
                // Startup: render logo as a ratatui widget (no markdown mangle).
                let model = app.ctx.providers.flash().map(|p| p.model_name()).unwrap_or("?");
                logo::render_logo(f, areas[1], env!("CARGO_PKG_VERSION"), model);
            } else {
                components::render_chat(
    f,
    areas[1],
    &app.messages,
    app.scroll_offset,
    &app.tree_registry,
    app.focused_tree,
);
            }
            components::render_divider(f, areas[3]);
            components::render_input(f, areas[4], &app.input_buffer);
            components::render_divider(f, areas[5]);

            // Calculate visual cursor position using Unicode width (Chinese chars = 2 columns).
            let prefix = app.input_buffer[..app.byte_index()].to_string();
            let visual_width = prefix.as_str().width();
            #[expect(clippy::cast_possible_truncation)]
            let cursor_x = areas[4].x + 2 + visual_width as u16;
            f.set_cursor_position(Position::new(cursor_x, areas[4].y));
        })?;

        app.tick = app.tick.wrapping_add(1);
        tokio::time::sleep(std::time::Duration::from_millis(16)).await;
    }

    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    execute!(terminal.backend_mut(), DisableBracketedPaste)?;
    disable_raw_mode()?;
    Ok(())
}
