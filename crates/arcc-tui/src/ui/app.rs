use std::sync::Arc;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
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
    pub ctx: SharedContext,

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
}

impl App {
    pub fn new(event_rx: mpsc::UnboundedReceiver<AppEvent>, ctx: SharedContext) -> Self {
        // Create an initial session for multi-turn conversation.
        let session = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(ctx.sessions.create("tui", "tui"))
        });
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
                self.delete_char();
            }
            AppEvent::Input(ch) if ch == "\x1b[D" => {
                // Left arrow
                self.character_index = self.character_index.saturating_sub(1);
            }
            AppEvent::Input(ch) if ch == "\x1b[C" => {
                // Right arrow
                let max = self.input_buffer.chars().count();
                if self.character_index < max {
                    self.character_index += 1;
                }
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
                debug!(lines, "scroll up");
                self.scroll_offset = self.scroll_offset.saturating_add(lines as usize);
            }
            AppEvent::ScrollDown(lines) => {
                debug!(lines, "scroll down");
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
            }
            AppEvent::HistoryPrev => {
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
            AppEvent::InteractiveCommand { .. } => {
                // Handled in the main event loop (needs terminal access).
            }
        }
    }

    /// Plan mode — streams a plan-focused prompt through the LLM with thinking mode enabled.
    fn plan_submit(&mut self, task: &str, tx: mpsc::UnboundedSender<AppEvent>) {
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

        tokio::spawn(async move {
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
                                Ok(StreamChunk::Finish(_)) | Ok(StreamChunk::ToolCallEnd { .. }) => {}
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

                    match confirm_choice {
                        ConfirmChoice::Reject => {
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
                        _ => {}
                    }

                    // Execute (skip_permissions=true because we already checked).
                    let ai_interactive = tc.arguments.get("interactive")
                        .and_then(|v| v.as_bool());
                    let lower = command.to_lowercase();
                    let words: Vec<&str> = lower.split_whitespace().collect();
                    let first = words.first().copied().unwrap_or("");
                    let auto_interactive = first == "sudo"
                        || words.contains(&"sudo")
                        || first == "ssh" || words.contains(&"ssh")
                        || first == "vim" || words.contains(&"vim")
                        || first == "nano" || words.contains(&"nano")
                        || first == "htop" || words.contains(&"htop")
                        || first == "top" || words.contains(&"top")
                        || first == "less" || words.contains(&"less")
                        || first == "more" || words.contains(&"more")
                        || first == "passwd" || words.contains(&"passwd")
                        || first == "telnet" || words.contains(&"telnet");
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
                        let r = tools::execute_command(&command, &*al, true).await;
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
    }

    /// Submit a prompt and handle the tool-calling stream loop.
    fn submit(&mut self, tx: mpsc::UnboundedSender<AppEvent>) {
        let prompt = std::mem::take(&mut self.input_buffer);
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() {
            return;
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

        tokio::spawn(async move {
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
                                Ok(StreamChunk::Finish(_)) | Ok(StreamChunk::ToolCallEnd { .. }) => {}
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

                    match confirm_choice {
                        ConfirmChoice::Reject => {
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
                        _ => {}
                    }

                    // Execute (skip_permissions=true because we already checked).
                    let ai_interactive = tc.arguments.get("interactive")
                        .and_then(|v| v.as_bool());
                    let lower = command.to_lowercase();
                    let words: Vec<&str> = lower.split_whitespace().collect();
                    let first = words.first().copied().unwrap_or("");
                    let auto_interactive = first == "sudo"
                        || words.contains(&"sudo")
                        || first == "ssh" || words.contains(&"ssh")
                        || first == "vim" || words.contains(&"vim")
                        || first == "nano" || words.contains(&"nano")
                        || first == "htop" || words.contains(&"htop")
                        || first == "top" || words.contains(&"top")
                        || first == "less" || words.contains(&"less")
                        || first == "more" || words.contains(&"more")
                        || first == "passwd" || words.contains(&"passwd")
                        || first == "telnet" || words.contains(&"telnet");
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
                        let r = tools::execute_command(&command, &*al, true).await;
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
                let new_session = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(self.ctx.sessions.create("tui", "tui"))
                });
                self.session = new_session;
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
                        tools::execute_command(&command, &*al, true).await
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
            "data" => {
                let sub = parts.get(1).copied().unwrap_or("help");
                let block = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        dbg_data(&self.ctx.storage, sub, &parts[2..]).await
                    })
                });
                match block {
                    Ok(lines) => self.messages.extend(lines),
                    Err(e) => self.messages.push(format!("⚠ data error: {e}")),
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

// ── `/data` subcommand handler ─────────────────────────────────────────

/// Parse an integer from `args[pos]` with a default, or an error message.
fn parse_opt(args: &[&str], pos: usize, default: usize, name: &str) -> Result<usize, String> {
    args.get(pos)
        .map(|s| s.parse::<usize>().map_err(|_| format!("invalid {name}: {s}")))
        .unwrap_or(Ok(default))
}

/// Dispatches `/data <sub> [args...]`.
async fn dbg_data(
    storage: &arcc_storage::ArccStorage,
    sub: &str,
    args: &[&str],
) -> Result<Vec<String>, String> {
    match sub {
        "sessions" => {
            let limit = parse_opt(args, 0, 10, "limit")?;
            let sessions = storage.list_sessions(limit).map_err(|e| e.to_string())?;
            if sessions.is_empty() {
                return Ok(vec!["🤖 No sessions found.".into()]);
            }
            let mut lines = vec!["🤖 **Recent sessions**".into(), "".into()];
            lines.push("| # | ID | Name | Mode | Last Active |".into());
            lines.push("|---|---|---|---|---|".into());
            for (i, s) in sessions.iter().enumerate() {
                let id_short = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
                lines.push(format!(
                    "| {} | `{}` | {} | {} | {} |",
                    i + 1,
                    id_short,
                    s.name,
                    s.mode,
                    &s.last_active_at[..19]
                ));
            }
            Ok(lines)
        }

        "messages" => {
            let session_id = args.first().ok_or("usage: `/data messages <session-id> [limit]`")?;
            let limit = parse_opt(args, 1, 20, "limit")?;
            let msgs = storage.session_messages(session_id, limit).map_err(|e| e.to_string())?;
            if msgs.is_empty() {
                return Ok(vec![format!("🤖 No messages for session `{session_id}`.")]);
            }
            let mut lines = vec![format!("🤖 **Messages** (session `{}`, last {})", &session_id[..8.min(session_id.len())], limit), String::new()];
            for m in msgs.iter().rev() {
                let preview: String = m.content.chars().take(200).collect();
                let ellipsis = if preview.len() < m.content.len() { "…" } else { "" };
                let tokens = m.token_count.map(|t| format!(" ({} tok)", t)).unwrap_or_default();
                lines.push(format!(
                    "> **{}**{} · {}",
                    m.role,
                    tokens,
                    m.created_at.as_deref().unwrap_or("?"),
                ));
                lines.push(format!("> {}", preview.replace('\n', "\\n")));
                lines.push(format!("{ellipsis}>"));
            }
            Ok(lines)
        }

        "token" => {
            let days = parse_opt(args, 0, 7, "days")?;
            let rows = storage.token_usage_daily(days).map_err(|e| e.to_string())?;
            let (total_in, total_out) = storage.total_tokens(days).map_err(|e| e.to_string())?;
            if rows.is_empty() {
                return Ok(vec![format!("🤖 No token usage recorded in the last {days} days.")]);
            }
            let mut lines = vec![
                format!("🤖 **Token usage — last {days} days**"),
                "".into(),
                "| Date | Model | Input | Output |".into(),
                "|---|---|---:|---:|".into(),
            ];
            for r in &rows {
                lines.push(format!("| {} | {} | {} | {} |", r.date, r.model, r.input_tokens, r.output_tokens));
            }
            lines.push("".into());
            lines.push(format!("🤖 **Total:** {} input / {} output tokens", total_in, total_out));
            Ok(lines)
        }

        "audit" => {
            let count = parse_opt(args, 0, 10, "count")?;
            let events = storage.recent_audit(count).map_err(|e| e.to_string())?;
            if events.is_empty() {
                return Ok(vec!["🤖 No audit events found.".into()]);
            }
            let mut lines = vec![format!("🤖 **Recent audit log** (last {})", events.len()), String::new()];
            for ev in &events {
                let (ts, label) = match ev {
                    AuditEvent::CommandExec { ts, cmd, risk, result, .. } => {
                        let ok = matches!(result, arcc_storage::audit::types::ExecResult::Ok);
                        let icon = if ok { "✅" } else { "❌" };
                        (ts.as_str(), format!("{icon} cmd  `{cmd}`  ({risk:?})"))
                    }
                    AuditEvent::CommandBlocked { ts, cmd, reason, .. } => {
                        (ts.as_str(), format!("🚫 blocked  `{cmd}`  ({reason})"))
                    }
                    AuditEvent::McpToolCall { ts, tool, result, .. } => {
                        let ok = matches!(result, arcc_storage::audit::types::ExecResult::Ok);
                        let icon = if ok { "✅" } else { "❌" };
                        (ts.as_str(), format!("{icon} mcp  `{tool}`"))
                    }
                    AuditEvent::HumanConfirm { ts, action, decision, .. } => {
                        let icon = matches!(decision, arcc_storage::audit::types::ConfirmDecision::Approved).then_some("👤✅").unwrap_or("👤❌");
                        (ts.as_str(), format!("{icon}  {action}  ({decision:?})"))
                    }
                };
                let short_ts = if ts.len() > 19 { &ts[..19] } else { ts };
                lines.push(format!("  `{short_ts}`  {label}"));
            }
            Ok(lines)
        }

        "summary" => {
            let session_id = args.first().ok_or("usage: `/data summary <session-id>`")?;
            match storage.latest_summary(session_id).map_err(|e| e.to_string())? {
                None => Ok(vec![format!("🤖 No summary for session `{session_id}`.")]),
                Some(s) => Ok(vec![
                    format!("🤖 **Conversation summary** (session `{}`)", &session_id[..8.min(session_id.len())]),
                    "".into(),
                    format!("> {}", s.summary_text.replace('\n', "\n> ")),
                    "".into(),
                    format!("🤖 Compressed at: {}", s.compressed_at.as_deref().unwrap_or("?")),
                ]),
            }
        }

        _ => {
            Ok(vec![
                "🤖 **`/data` subcommands**".into(),
                "".into(),
                "| Command | Description |".into(),
                "|---|---|".into(),
                "| `/data sessions [limit]` | List recent sessions |".into(),
                "| `/data messages <id> [limit]` | Show session messages |".into(),
                "| `/data token [days]` | Token consumption summary |".into(),
                "| `/data audit [count]` | Recent audit log entries |".into(),
                "| `/data summary <id>` | Show compressed summary |".into(),
            ])
        }
    }
}

// ---------------------------------------------------------------------------
// Main TUI loop
// ---------------------------------------------------------------------------

/// Run the TUI main loop — Claude Code CLI style.
pub async fn run(ctx: SharedContext) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    let (tx, rx) = crate::event::loop_event::create_event_loop();
    let mut input_handle = crate::event::handler::spawn_input_handler(tx.clone());
    let mut app = App::new(rx, ctx);

    while app.running {
        while let Ok(event) = app.event_rx.try_recv() {
            match event {
                AppEvent::Input(ch) if ch == "\n" || ch == "\r" => {
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
            if app.messages.is_empty() {
                // Startup: render logo as a ratatui widget (no markdown mangle).
                let model = app.ctx.providers.flash().map(|p| p.model_name()).unwrap_or("?");
                logo::render_logo(f, areas[1], env!("CARGO_PKG_VERSION"), model);
            } else {
                components::render_chat(f, areas[1], &app.messages, app.scroll_offset);
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
    disable_raw_mode()?;
    Ok(())
}
