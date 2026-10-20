use arcc_storage::audit::types::{Approval, AuditEvent, ConfirmDecision, ExecResult, RiskLevel};
use arcc_storage::config::loader::ComputerUseConfig;
use chrono::Utc;
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::layout::Position;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::{debug, error, info, warn};

use super::components;
use super::logo;
use crate::commands;
use arcc_core::computer;
use arcc_core::context::SharedContext;
use arcc_core::model::provider::ModelProvider;
use arcc_core::model::types::{ChatMessage, ChatRequest, StreamChunk, ToolCall};
use arcc_core::session::Session;
use arcc_core::tools;
use serde_json;
use unicode_width::UnicodeWidthStr;

use crate::event::loop_event::{AppEvent, PromptRequest};

/// Maximum App.messages entries before oldest are pruned.
/// Kept at 2× the render cap (MAX_VISIBLE_MSGS=80) so scroll-back
/// still has useful range while keeping memory bounded.
const MAX_STORED_MSGS: usize = 160;

/// Time-to-live for cached dashboard data (avoids redundant subprocess calls).
const DASHBOARD_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// Cached dashboard data with a collection timestamp.
struct DashboardCache {
    data: components::DashboardData,
    collected_at: Instant,
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

    // ── Background task handles ──
    /// Handle of the currently running AI response task (Esc to abort).
    pub task_handle: Option<tokio::task::JoinHandle<()>>,
    /// Handle of the /init generation task (cancelled on new submit).
    pub init_handle: Option<tokio::task::JoinHandle<()>>,

    // ── Multi-turn conversation & thinking mode ──
    pub session: Arc<RwLock<Session>>,
    pub thinking_mode: bool,
    pub reasoning_content: String,
    /// Desktop control enabled for this TUI session (`/computer on|off`).
    pub computer_enabled: Arc<AtomicBool>,
    /// User answered "a" — skip further computer-action confirmations.
    pub computer_trusted: Arc<AtomicBool>,

    // ── Chat scroll state ──
    /// Lines to scroll the chat area upward (0 = top/auto-follow).
    pub scroll_offset: usize,

    /// Whether terminal mouse capture is active.
    ///
    /// Capture enables wheel scrolling but blocks native drag-to-select;
    /// `/mouse off` releases it.  Consulted when re-entering the TUI after an
    /// interactive child command so the user's choice is preserved.
    pub mouse_capture: bool,

    // ── Interactive prompt ──
    /// Non-None when the user is being prompted for input.
    /// Used by cow confirm, /init overwrite, and any future y/n prompts.
    pub pending_prompt: Option<PromptRequest>,

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
    /// Cached dashboard data with TTL to avoid redundant subprocess calls.
    dashboard_cache: Option<DashboardCache>,
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
        // In-memory session — persisted to SQLite lazily on first LLM response
        // (see `enable_persistence` in `run_tool_calling_loop`).
        let context_max = ctx.storage.config.model.context_max_tokens;
        let session = Arc::new(RwLock::new(arcc_core::session::Session::new(
            &uuid::Uuid::new_v4().to_string(),
            "tui",
            "tui",
            context_max,
            None,
        )));
        // Load recent input history from DB (best-effort, oldest-first order).
        let loaded_history: Vec<String> = ctx
            .storage
            .recent_input_history(200)
            .ok()
            .map(|entries| entries.into_iter().rev().map(|e| e.prompt).collect())
            .unwrap_or_default();
        let computer_enabled = Arc::new(AtomicBool::new(ctx.storage.config.computer_use.enabled));
        Self {
            input_buffer: String::new(),
            character_index: 0,
            messages: Vec::new(),
            status: components::status::IDLE.into(),
            tick: 0,
            running: true,
            session,
            thinking_mode: false,
            reasoning_content: String::new(),
            computer_enabled,
            computer_trusted: Arc::new(AtomicBool::new(false)),
            scroll_offset: 0,
            mouse_capture: true,
            pending_prompt: None,
            input_history: loaded_history,
            history_index: 0,
            completion_candidates: Vec::new(),
            completion_index: 0,
            tab_active: false,
            blink: 0,
            task_handle: None,
            init_handle: None,
            show_dashboard: false,
            dashboard: None,
            dashboard_cache: None,
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

    /// Whether the user can edit the input buffer.
    ///
    /// During EXECUTING the subprocess has stdin — stray keystrokes from
    /// /dev/tty leaks (e.g. sudo prompts) must be discarded.
    fn can_edit(&self) -> bool {
        self.status == components::status::IDLE
            || self.status == components::status::THINKING
            || self.status == components::status::STREAMING
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Input(ch) if ch == "\n" || ch == "\r" => {}
            AppEvent::Input(ch) if ch == "\x08" || ch == "\x7f" => {
                if self.can_edit() {
                    self.delete_char();
                }
            }
            AppEvent::Input(ch) if ch == "\x1b[D" => {
                if self.can_edit() {
                    self.character_index = self.character_index.saturating_sub(1);
                }
            }
            AppEvent::Input(ch) if ch == "\x1b[C" => {
                if self.can_edit() {
                    let max = self.input_buffer.chars().count();
                    if self.character_index < max {
                        self.character_index += 1;
                    }
                }
            }
            AppEvent::Input(_ch) if self.status == components::status::EXECUTING => {
                // Subprocess running — discard stray keystrokes that may come
                // from subprocesses writing to /dev/tty (e.g. sudo password
                // prompts leaking through the TUI's raw-mode input handler).
            }
            AppEvent::Input(ch) => {
                for c in ch.chars() {
                    // Skip control characters (C0: 0x00-0x1F and DEL: 0x7F)
                    // that some terminals may send as Char events instead of
                    // Backspace.  When not filtered, \x7f gets inserted as
                    // visible garbage in the input buffer.
                    // Note: c.is_control() also matches \x7f on most platforms.
                    if c.is_control() {
                        continue;
                    }
                    self.insert_char(c);
                }
                self.tab_active = false;
            }
            AppEvent::Prompt(req) => {
                self.messages.push(req.message.clone());
                self.pending_prompt = Some(req);
                self.status = components::status::WAITING.into();
            }
            AppEvent::PromptExpired => {
                if let Some(pending) = self.pending_prompt.take() {
                    let _ = pending.response_tx.send(None);
                }
                self.input_buffer.clear();
                self.character_index = 0;
                self.status = components::status::IDLE.into();
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
                    let max_rows = self
                        .dashboard
                        .as_ref()
                        .map_or(0, |d| d.session_ids.len().saturating_sub(1));
                    let step = lines as usize;
                    self.dashboard_cursor = (self.dashboard_cursor + step).min(max_rows);
                    let body_h = 8;
                    if self.dashboard_cursor >= self.dashboard_scroll + body_h {
                        self.dashboard_scroll = self
                            .dashboard_cursor
                            .saturating_sub(body_h)
                            .saturating_add(1);
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
                self.status = components::status::STREAMING.into();
                self.scroll_offset = 0;
            }
            AppEvent::ToolExec(text) => {
                self.messages.push(format!("⚡ {text}"));
                self.scroll_offset = 0;
                self.status = components::status::EXECUTING.into();
            }
            AppEvent::StreamDone => {
                self.status = components::status::IDLE.into();
                self.blink = 0;
                self.reasoning_content.clear();
                // User may have been typing while AI was responding — preserve buffer.
                // Only clear if a pending prompt is aborted by a new conversation.
                if let Some(pending) = self.pending_prompt.take() {
                    let _ = pending.response_tx.send(None);
                    self.input_buffer.clear();
                    self.character_index = 0;
                }
                // Trigger background context compression if session is over threshold.
                let ctx = self.ctx.clone();
                let tx = self.event_tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(AppEvent::Status(components::status::COMPRESSING.into()));
                    if let Some(flash) = ctx.providers.flash() {
                        ctx.sessions.compress_all(&**flash).await;
                    }
                    let _ = tx.send(AppEvent::Status(components::status::IDLE.into()));
                });
            }
            AppEvent::HistoryPrev => {
                // When dashboard is shown, move cursor up in sessions table.
                if self.show_dashboard {
                    if self.dashboard_cursor > 0 {
                        self.dashboard_cursor -= 1;
                        if self.dashboard_cursor < self.dashboard_scroll {
                            self.dashboard_scroll = self.dashboard_cursor;
                        }
                    }
                    return;
                }
                // ↑ always navigates history (global — use PgUp/PgDn to scroll chat).
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
                        let body_h = 8;
                        if self.dashboard_cursor >= self.dashboard_scroll + body_h {
                            self.dashboard_scroll = self
                                .dashboard_cursor
                                .saturating_sub(body_h)
                                .saturating_add(1);
                        }
                    }
                    return;
                }
                // ↓ always navigates history (global — use PgUp/PgDn to scroll chat).
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
                            Some(p) if p + 1 < hashes.len() => {
                                self.focused_tree = Some(hashes[p + 1])
                            }
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
                        self.completion_candidates = commands::complete(cmd_prefix)
                            .into_iter()
                            .map(|s| s.to_string())
                            .collect();
                        self.completion_index = 0;
                        self.tab_active = !self.completion_candidates.is_empty();
                    }
                    if !self.completion_candidates.is_empty() {
                        let name = &self.completion_candidates[self.completion_index];
                        self.input_buffer = format!("/{name} ");
                        self.character_index = self.input_buffer.chars().count();
                        self.completion_index =
                            (self.completion_index + 1) % self.completion_candidates.len();
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
                self.status = components::status::THINKING.into();
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
                } else if self.status != components::status::IDLE {
                    // Esc during AI response: abort the streaming task.
                    info!("user aborted AI response");
                    if let Some(handle) = self.task_handle.take() {
                        handle.abort();
                    }
                    self.status = components::status::IDLE.into();
                    self.messages.push("🤖 _(stopped)_".into());
                    self.blink = 0;
                } else if self.show_dashboard {
                    debug!("dismissing dashboard");
                    self.show_dashboard = false;
                    self.dashboard_scroll = 0;
                    self.dashboard_cursor = 0;
                    stop_monitor(&mut self.monitor_handle);
                } else if self.dashboard.is_some()
                    && self
                        .messages
                        .last()
                        .is_some_and(|m| m.contains("Esc to return"))
                {
                    // Re-open dashboard after viewing session messages
                    self.show_dashboard = true;
                    info!("returning to dashboard");
                }
            }
            AppEvent::InteractiveCommand { .. } => {
                // Handled in the main event loop (needs terminal access).
            }
            AppEvent::SetMouseCapture(enabled) => {
                // The main loop also flips the terminal mode; keep the mirror
                // in sync in case the event is routed here.
                self.mouse_capture = enabled;
            }
            AppEvent::Status(text) => {
                self.status = text;
            }
        }
    }

    /// Plan mode — streams a plan-focused prompt through the LLM with thinking mode enabled.
    fn plan_submit(&mut self, task: &str, tx: mpsc::UnboundedSender<AppEvent>) {
        // Cancel any running /init generation.
        if let Some(h) = self.init_handle.take() {
            h.abort();
        }
        self.thinking_mode = true;
        self.messages.push(format!("🧑 /plan {task}"));
        self.status = components::status::PLANNING.into();

        let provider = match self.ctx.providers.pick(task, true) {
            Some(p) => p.clone(),
            None => {
                self.messages.push("🤖 No model provider available.".into());
                return;
            }
        };

        // ── Persist plan request to session ──
        {
            let session_id = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { self.session.read().await.id.clone() })
            });
            let user_msg = ChatMessage::user(format!("Plan the following task:\n\n{task}"));
            {
                let mut s = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(self.session.write())
                });
                s.push_message(user_msg, provider.count_tokens(task));
            }
            info!(session = %session_id, plan_task_len = task.len(), "plan request submitted");
        }

        let plan_system = {
            let mut msg = arcc_core::model::prompts::templates::plan(task).to_chat_message();
            let instr = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { self.ctx.project_instructions.read().await.clone() })
            });
            if let Some(text) = instr {
                msg.content.push_str("\n\n## Project Instructions\n\n");
                msg.content.push_str(&text);
            }
            msg
        };

        let session = self.session.clone();
        let plan_system_content = plan_system.content.clone();

        // An aborted computer turn may leave a stale screenshot attached.
        {
            let mut s = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(session.write())
            });
            s.prune_images(0);
        }
        let history_msgs = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { session.read().await.prepare_for_request() })
        });
        let initial_messages: Vec<ChatMessage> =
            std::iter::once(plan_system).chain(history_msgs).collect();

        let config = ToolCallLoopConfig {
            system_content: plan_system_content,
            request_thinking: false,
            computer_enabled: None,
            computer_trusted: Arc::new(AtomicBool::new(false)),
            computer_config: self.ctx.storage.config.computer_use.clone(),
            vision_model: self.vision_model_name(),
        };
        self.task_handle = Some(tokio::spawn(run_tool_calling_loop(
            provider,
            self.session.clone(),
            self.ctx.clone(),
            config,
            initial_messages,
            tx,
        )));
    }

    /// Submit a prompt and handle the tool-calling stream loop.
    fn submit(&mut self, tx: mpsc::UnboundedSender<AppEvent>) {
        // Cancel any running /init generation — user started a new conversation.
        if let Some(h) = self.init_handle.take() {
            h.abort();
        }
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

        // ── Persist input history (best-effort, before any early return) ──
        {
            let session_id = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { self.session.read().await.id.clone() })
            });
            if let Err(e) = self.ctx.storage.record_input_history(&session_id, &prompt) {
                warn!(err = %e, "failed to persist input history");
            }
        }

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

        // Session persistence is deferred to `run_tool_calling_loop` —
        // it calls `enable_persistence` on the first LLM response chunk.

        self.messages.push(format!("🧑 {prompt}"));
        self.status = components::status::THINKING.into();
        self.scroll_offset = 0;

        let provider = match self.ctx.providers.pick(&prompt, true) {
            Some(p) => p.clone(),
            None => {
                self.messages.push("🤖 No model provider available.".into());
                self.status = components::status::ERROR.into();
                return;
            }
        };

        // ── Persist user input to session ──
        let session_id = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.session.read().await.id.clone() })
        });
        let user_msg = ChatMessage::user(prompt.clone());
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

        let thinking_mode = self.thinking_mode;
        let session = self.session.clone();

        let system_msg = {
            let mut msg = arcc_core::model::prompts::templates::tui().to_chat_message();
            let instr = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { self.ctx.project_instructions.read().await.clone() })
            });
            if let Some(text) = instr {
                msg.content.push_str("\n\n## Project Instructions\n\n");
                msg.content.push_str(&text);
            }
            if self.computer_enabled.load(Ordering::Relaxed) {
                msg.content.push_str("\n\n");
                msg.content
                    .push_str(arcc_core::model::prompts::templates::computer().as_str());
            }
            msg
        };

        // An aborted computer turn may leave a stale screenshot attached.
        {
            let mut s = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(session.write())
            });
            s.prune_images(0);
        }
        let history_msgs = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { session.read().await.prepare_for_request() })
        });
        let initial_messages: Vec<ChatMessage> = std::iter::once(system_msg.clone())
            .chain(history_msgs)
            .collect();

        info!("TUI: starting multi-turn tool-calling stream");

        let config = ToolCallLoopConfig {
            system_content: system_msg.content.clone(),
            request_thinking: thinking_mode,
            computer_enabled: Some(self.computer_enabled.clone()),
            computer_trusted: self.computer_trusted.clone(),
            computer_config: self.ctx.storage.config.computer_use.clone(),
            vision_model: self.vision_model_name(),
        };
        self.task_handle = Some(tokio::spawn(run_tool_calling_loop(
            provider,
            self.session.clone(),
            self.ctx.clone(),
            config,
            initial_messages,
            tx,
        )));
    }

    /// Provider name used for image-bearing requests.
    fn vision_model_name(&self) -> String {
        let cfg = &self.ctx.storage.config.computer_use;
        if cfg.vision_model.is_empty() {
            self.ctx.storage.config.model.flash_model.clone()
        } else {
            cfg.vision_model.clone()
        }
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
                        self.messages
                            .push(format!("🤖 **{}**  — {}", c.usage, c.desc));
                        self.messages
                            .push(format!("🤖 Category: **{}**", c.cat.label()));
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
                info!("session cleared — new UUID");
                if let Some(pending) = self.pending_prompt.take() {
                    let _ = pending.response_tx.send(None);
                }
                self.messages.clear();
                self.reasoning_content.clear();
                self.computer_trusted.store(false, Ordering::Relaxed);
                // Create a new session with a fresh UUID so the AI sees
                // zero prior context.  DB persistence is preserved.
                if let Ok(mut s) = self.session.try_write() {
                    s.reset(&uuid::Uuid::new_v4().to_string());
                }
            }
            "model" => {
                let flash = self
                    .ctx
                    .providers
                    .flash()
                    .map(|p| p.model_name())
                    .unwrap_or("?");
                let pro = self
                    .ctx
                    .providers
                    .pro()
                    .map(|p| p.model_name())
                    .unwrap_or("?");
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
                    self.messages
                        .push("🤖   (none registered — use /exec to run commands directly)".into());
                } else {
                    for t in &tools {
                        self.messages
                            .push(format!("🤖   - **{}**: {}", t.name, t.description));
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
                    self.messages
                        .push(format!("⚠ Command blocked by allowlist: `{command}`"));
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
                let timeout_s = self.ctx.storage.config.execution.command_timeout_seconds;
                let max_bytes = self.ctx.storage.config.execution.max_output_bytes;
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let al = self.ctx.allowlist.read().await;
                        tools::execute_command_with_config(
                            &command, &al, true, timeout_s, max_bytes,
                        )
                        .await
                    })
                });

                match result {
                    Ok(out) => {
                        let content = out.to_content();
                        self.messages
                            .push(format!("⚡ exit={:?}\n```\n{content}\n```", out.exit_code));
                    }
                    Err(e) => {
                        self.messages.push(format!("⚡ {e}"));
                    }
                }
            }
            "computer" => {
                let sub = parts.get(1).copied().unwrap_or("status");
                match sub {
                    "on" => {
                        if !computer::is_supported() {
                            self.messages
                                .push("🤖 Desktop control is currently macOS-only.".into());
                            return;
                        }
                        self.computer_enabled.store(true, Ordering::Relaxed);
                        self.computer_trusted.store(false, Ordering::Relaxed);
                        self.messages.push("🤖 Computer use: **on**".into());
                        self.messages.push(
                            "🤖 Screenshots are sent to the DeepSeek API. Mouse/keyboard \
                             actions require confirmation (y/a/n); `a` trusts this session."
                                .into(),
                        );
                        self.messages.push(
                            "🤖 Grant **Screen Recording** and **Accessibility** to your \
                             terminal in System Settings → Privacy & Security, then restart \
                             the terminal if actions do nothing."
                                .into(),
                        );
                    }
                    "off" => {
                        self.computer_enabled.store(false, Ordering::Relaxed);
                        self.computer_trusted.store(false, Ordering::Relaxed);
                        if let Ok(mut session) = self.session.try_write() {
                            session.prune_images(0);
                        }
                        self.messages.push("🤖 Computer use: **off**".into());
                    }
                    "status" => {
                        let enabled = self.computer_enabled.load(Ordering::Relaxed);
                        let trusted = self.computer_trusted.load(Ordering::Relaxed);
                        let cfg = &self.ctx.storage.config.computer_use;
                        let vision = if cfg.vision_model.is_empty() {
                            self.ctx.storage.config.model.flash_model.clone()
                        } else {
                            cfg.vision_model.clone()
                        };
                        let platform = if computer::is_supported() {
                            "macOS (supported)"
                        } else {
                            "unsupported on this platform"
                        };
                        self.messages.push(format!(
                            "🤖 Computer use: **{}**",
                            if enabled { "on" } else { "off" }
                        ));
                        self.messages.push(format!(
                            "🤖 Session trust: **{}**",
                            if trusted { "on" } else { "off" }
                        ));
                        self.messages.push(format!("🤖 Platform: {platform}"));
                        self.messages.push(format!("🤖 Vision model: `{vision}`"));
                        self.messages.push(
                            "🤖 Permissions: Screen Recording + Accessibility in System \
                             Settings → Privacy & Security."
                                .into(),
                        );
                    }
                    other => self.messages.push(format!(
                        "🤖 Usage: `/computer [on/off/status]` (unknown subcommand: {other})"
                    )),
                }
            }
            "thinking" => {
                self.thinking_mode = !self.thinking_mode;
                let state = if self.thinking_mode { "on" } else { "off" };
                info!(thinking = state, "thinking mode toggled");
                self.messages.push(format!("🤖 Thinking mode: **{state}**"));
            }
            "mouse" => {
                let sub = parts.get(1).copied().unwrap_or("status");
                match sub {
                    "on" => {
                        self.mouse_capture = true;
                        let _ = self.event_tx.send(AppEvent::SetMouseCapture(true));
                        info!("mouse capture enabled");
                        self.messages.push(
                            "🤖 Mouse capture: **on** — wheel scrolls the chat. \
                             Hold `Shift` (or `Option` in Terminal.app/iTerm2) while \
                             dragging to select text."
                                .into(),
                        );
                    }
                    "off" => {
                        self.mouse_capture = false;
                        let _ = self.event_tx.send(AppEvent::SetMouseCapture(false));
                        info!("mouse capture disabled");
                        self.messages.push(
                            "🤖 Mouse capture: **off** — drag to select and copy with the \
                             terminal's native behavior."
                                .into(),
                        );
                        self.messages.push(
                            "🤖 Wheel is handed back to the terminal; use `PgUp`/`PgDn` \
                             (or `Home`/`End`) to scroll the chat. `/mouse on` restores \
                             wheel scrolling."
                                .into(),
                        );
                    }
                    "status" => {
                        let state = if self.mouse_capture { "on" } else { "off" };
                        self.messages
                            .push(format!("🤖 Mouse capture: **{state}**"));
                    }
                    other => self.messages.push(format!(
                        "🤖 Usage: `/mouse [on/off/status]` (unknown subcommand: {other})"
                    )),
                }
            }
            "stats" => {
                let msg_count = self.messages.len();
                let history_count = self.input_history.len();
                let session_id = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(async { self.session.read().await.id.clone() })
                });
                info!(msg_count, history_count, %session_id, "stats");
                self.messages.push(format!("🤖 Messages: {msg_count}"));
                self.messages
                    .push(format!("🤖 History: {history_count} entries"));
                self.messages.push(format!("🤖 Session: `{session_id}`"));
            }
            "dashboard" => {
                self.show_dashboard = !self.show_dashboard;
                if self.show_dashboard {
                    // Use cached data if still within TTL.
                    let data = match &self.dashboard_cache {
                        Some(cache) if cache.collected_at.elapsed() < DASHBOARD_CACHE_TTL => {
                            cache.data.clone()
                        }
                        _ => {
                            let data = collect_dashboard_data(&self.ctx);
                            self.dashboard_cache = Some(DashboardCache {
                                data: data.clone(),
                                collected_at: Instant::now(),
                            });
                            data
                        }
                    };
                    self.dashboard = Some(data);
                    self.dashboard_scroll = 0;
                    self.dashboard_cursor = 0;
                    info!("dashboard opened");
                    start_monitor(&mut self.monitor_handle, self.event_tx.clone());
                } else {
                    stop_monitor(&mut self.monitor_handle);
                }
            }
            "init" => {
                // Detect project root.
                let cwd = std::env::current_dir().unwrap_or_default();
                let root = std::process::Command::new("git")
                    .args(["rev-parse", "--show-toplevel"])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| cwd.to_string_lossy().to_string());
                let path = std::path::Path::new(&root).join("AGENTS.md");
                if path.exists() {
                    let (resp_tx, resp_rx) = oneshot::channel();
                    let _ = self.event_tx.send(AppEvent::Prompt(PromptRequest {
                        message: cow_say("Overwrite AGENTS.md?", "[y] yes · [n] no"),
                        hint: String::new(),
                        response_tx: resp_tx,
                    }));
                    // Spawn a task to await the response.
                    let ctx = self.ctx.clone();
                    let tx = self.event_tx.clone();
                    self.init_handle = Some(tokio::spawn(async move {
                        if let Ok(Some(answer)) = resp_rx.await {
                            if answer == "y" || answer == "yes" {
                                let _ =
                                    tx.send(AppEvent::ToolExec("generating AGENTS.md...".into()));
                                let cwd = std::env::current_dir().unwrap_or_default();
                                let root = std::process::Command::new("git")
                                    .args(["rev-parse", "--show-toplevel"])
                                    .output()
                                    .ok()
                                    .and_then(|o| String::from_utf8(o.stdout).ok())
                                    .map(|s| s.trim().to_owned())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| cwd.to_string_lossy().to_string());
                                let agents_path = std::path::Path::new(&root).join("AGENTS.md");
                                run_project_init(ctx, tx, &agents_path, &root).await;
                            } else {
                                let _ =
                                    tx.send(AppEvent::Token("🤖 Kept existing AGENTS.md.".into()));
                            }
                        }
                    }));
                    return;
                }

                self.status = components::status::THINKING.into();
                self.messages
                    .push("🤖 Analyzing project structure — this may take a moment...".into());
                self.scroll_offset = 0;

                let ctx = self.ctx.clone();
                let tx = self.event_tx.clone();
                let agents_path = path.clone();
                let root_str = root.clone();
                self.init_handle = Some(tokio::spawn(async move {
                    run_project_init(ctx, tx, &agents_path, &root_str).await;
                }));
            }
            "exit" | "quit" => {
                info!("user quit via command");
                self.running = false;
            }
            _ => {
                self.messages
                    .push(format!("🤖 Unknown command: `{input}` — try `/help`"));
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
    let mem = arcc_core::system::memory_info();

    let total_tokens = ctx.storage.total_tokens(7).unwrap_or((0, 0));
    let (total_input, total_output) = total_tokens;

    // Sessions
    let sessions = ctx.storage.list_sessions(50).unwrap_or_default();
    let session_count = sessions.len();
    let total_msgs: usize = ctx.storage.message_count().unwrap_or(0) as usize;

    // Format session rows for table display
    let session_rows: Vec<String> = sessions
        .iter()
        .map(|s| {
            let id_short = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
            let active = fmt_local_ts(&s.last_active_at);
            format!("{}|{}|{}|{}", id_short, s.name, s.mode, active)
        })
        .collect();
    let session_ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();

    // Daily token usage
    let token_daily = ctx.storage.token_usage_daily(7).unwrap_or_default();
    let chart_data: Vec<(String, u64)> = token_daily
        .iter()
        .map(|r| {
            let label = if r.date.len() >= 10 {
                r.date[8..10].trim_start_matches('0').to_owned() // "11"
            } else {
                r.date.clone()
            };
            let total = (r.input_tokens + r.output_tokens) as u64;
            (label, total)
        })
        .collect();

    // Recent audit events
    let audit = ctx.storage.recent_audit(10).unwrap_or_default();
    let audit_items: Vec<(String, String, bool)> = audit
        .iter()
        .map(|ev| match ev {
            arcc_storage::audit::types::AuditEvent::CommandExec {
                ts, cmd, result, ..
            } => {
                let ok = matches!(result, arcc_storage::audit::types::ExecResult::Ok);
                let ts = fmt_local_ts(ts);
                (ts, format!("cmd {cmd}"), ok)
            }
            arcc_storage::audit::types::AuditEvent::CommandBlocked { ts, cmd, .. } => {
                let ts = fmt_local_ts(ts);
                (ts, format!("blocked {cmd}"), false)
            }
            arcc_storage::audit::types::AuditEvent::McpToolCall {
                ts, tool, result, ..
            } => {
                let ok = matches!(result, arcc_storage::audit::types::ExecResult::Ok);
                let ts = fmt_local_ts(ts);
                (ts, format!("mcp {tool}"), ok)
            }
            arcc_storage::audit::types::AuditEvent::HumanConfirm {
                ts,
                action,
                decision,
                ..
            } => {
                let ok = matches!(
                    decision,
                    arcc_storage::audit::types::ConfirmDecision::Approved
                );
                let ts = fmt_local_ts(ts);
                (ts, format!("confirm {action}"), ok)
            }
            arcc_storage::audit::types::AuditEvent::ComputerAction {
                ts, action, result, ..
            } => {
                let ok = matches!(result, arcc_storage::audit::types::ExecResult::Ok);
                let ts = fmt_local_ts(ts);
                (ts, format!("computer {action}"), ok)
            }
        })
        .collect();

    components::DashboardData {
        system: components::SystemInfo {
            hostname,
            os: format!("{os} ({arch})"),
            cpu_count,
            uptime: uptime.unwrap_or_else(|| "?".into()),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            memory_total_mb: mem.total_mb(),
            memory_used_mb: mem.used_mb(),
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

/// Parse an RFC 3339 or SQLite timestamp and format as local `YYYY-MM-DD HH:MM:SS`.
fn fmt_local_ts(ts: &str) -> String {
    // RFC 3339 with timezone (e.g. "2026-06-11T01:26:37.630464Z")
    if let Ok(dt) = ts.parse::<chrono::DateTime<chrono::Utc>>() {
        return dt
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
    }
    // SQLite format (no timezone — interpret as UTC)
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S") {
        let utc: chrono::DateTime<chrono::Utc> =
            chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc);
        return utc
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
    }
    // Fallback: return as-is (strip T if present)
    ts.replace('T', " ")
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
            let (cpu_pct, mem_pct, rx_bytes, tx_bytes) = tokio::task::spawn_blocking(move || {
                let c = get_live_cpu(core_count);
                let m = arcc_core::system::memory_info().usage_pct();
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
            let total: f64 = output
                .lines()
                .filter_map(|l| {
                    let s = l.trim().replace(',', ".");
                    s.parse::<f64>().ok()
                })
                .sum();
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
    let ib_idx = hcols
        .iter()
        .position(|&c| c.contains("Ibytes") || c.contains("ibytes"));
    let ob_idx = hcols
        .iter()
        .position(|&c| c.contains("Obytes") || c.contains("obytes"));
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
        let ib = cols
            .get(ib_idx)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let ob = cols
            .get(ob_idx)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if ib > 0 || ob > 0 {
            return (ib, ob);
        }
    }
    (0, 0)
}

// ---------------------------------------------------------------------------
// Shared tool-calling loop — extracted from plan_submit / submit
// ---------------------------------------------------------------------------

/// Parameters that differentiate `plan_submit` from `submit` in the shared loop.
#[derive(Clone)]
struct ToolCallLoopConfig {
    /// System message content used for phase-2 message reconstruction.
    system_content: String,
    /// Whether to request thinking mode in the ChatRequest.
    request_thinking: bool,
    /// `None` disables computer-use entirely (e.g. `/plan`).
    computer_enabled: Option<Arc<AtomicBool>>,
    /// Set after the user answers `a` to a computer-action confirmation.
    computer_trusted: Arc<AtomicBool>,
    computer_config: ComputerUseConfig,
    /// Provider used when the request contains screenshots.
    vision_model: String,
}

/// Multi-turn tool-calling stream loop shared by `plan_submit` and `submit`.
///
/// Handles:
/// 1. Stream consumption (Content / Reasoning / ToolCallStart / Finish)
/// 2. Token usage recording
/// 3. Assistant message persistence to session
/// 4. Permission-checking + tool execution + result persistence
/// 5. Phase-2 message reconstruction
async fn run_tool_calling_loop(
    provider: Arc<dyn ModelProvider>,
    session: Arc<RwLock<Session>>,
    ctx: SharedContext,
    config: ToolCallLoopConfig,
    initial_messages: Vec<ChatMessage>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let temperature = ctx.storage.config.model.temperature;
    let max_tokens = ctx.storage.config.model.max_output_tokens;
    let skip_permissions = ctx.dangerously_skip_permissions;

    let mut messages = initial_messages;
    let mut phase = 1u32;
    let mut round: u32 = 0;
    let mut first_iteration = true;
    let mut computer_session = computer::ComputerSession::default();

    loop {
        let computer_active = config
            .computer_enabled
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed));
        let has_tools = phase == 1;

        if first_iteration {
            first_iteration = false;
        } else {
            // Drop stale screenshots and rebuild from the session so tool
            // results pushed in the previous round are part of this request.
            {
                let mut s = session.write().await;
                s.prune_images(config.computer_config.keep_screenshots);
            }
            messages = {
                let s = session.read().await;
                let mut base = s.prepare_for_request();
                base.retain(|m| m.role != "system");
                std::iter::once(ChatMessage::system(config.system_content.clone()))
                    .chain(base)
                    .collect()
            };
        }

        // Screenshots require a vision-capable model. DeepSeek Pro is
        // text-only, so route image-bearing requests to the configured
        // vision provider (Flash by default) and fall back gracefully.
        let has_images = messages.iter().any(ChatMessage::has_images);
        let active_provider = if has_images {
            ctx.providers
                .get(&config.vision_model)
                .or_else(|| ctx.providers.flash())
                .cloned()
                .unwrap_or_else(|| provider.clone())
        } else {
            provider.clone()
        };

        let mut tool_defs = vec![tools::command_tool_definition()];
        if has_tools && computer_active && computer::is_supported() {
            tool_defs.push(computer::tool_definition());
        }

        let req = ChatRequest {
            model: active_provider.model_name().to_owned(),
            messages: messages.clone(),
            tools: if has_tools { Some(tool_defs) } else { None },
            tool_choice: if has_tools {
                Some(serde_json::json!("auto"))
            } else {
                None
            },
            temperature: Some(temperature),
            max_tokens: Some(max_tokens),
            stream: true,
            thinking_mode: if config.request_thinking {
                Some("enabled".into())
            } else {
                None
            },
            reasoning_effort: if config.request_thinking {
                Some("max".into())
            } else {
                None
            },
        };

        let mut persisted = false;
        let mut content_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut last_usage: Option<arcc_core::model::types::Usage> = None;

        match active_provider.chat_stream(req).await {
            Ok(stream) => {
                let mut stream = Box::pin(stream);
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(StreamChunk::Content(text)) => {
                            // Persist session to SQLite on the very first
                            // LLM content chunk — guarantees we never write
                            // an empty session to the database.
                            if !persisted {
                                persisted = true;
                                session
                                    .write()
                                    .await
                                    .enable_persistence(ctx.storage.db.clone());
                                info!("session persisted on first LLM response");
                            }
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

        // Record token usage from API response.
        if let Some(ref usage) = last_usage {
            let sid = session.read().await.id.clone();
            let mdl = active_provider.model_name().to_owned();
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
                images: Vec::new(),
                tool_calls: if has_tool_calls {
                    Some(tool_calls.clone())
                } else {
                    None
                },
                tool_call_id: None,
                reasoning_content: if reasoning_buf.is_empty() {
                    None
                } else {
                    Some(reasoning_buf.clone())
                },
            };
            s.push_message(
                assistant_msg,
                active_provider.count_tokens(&content_buf)
                    + active_provider.count_tokens(&reasoning_buf),
            );
        }

        if !has_tool_calls {
            let _ = tx.send(AppEvent::StreamDone);
            return;
        }

        // Execute tool calls.
        let mut last_screenshot: Option<(
            arcc_core::model::types::ImageAttachment,
            Option<computer::ScreenGeometryInfo>,
        )> = None;

        for tc in &tool_calls {
            info!(tool = %tc.name, id = %tc.id, "executing tool call");

            match tc.name.as_str() {
                "execute_command" => {
                    let command = tc.arguments["command"].as_str().unwrap_or("").to_owned();
                    let cmd_name = command
                        .split_whitespace()
                        .next()
                        .unwrap_or(&command)
                        .to_string();
                    let _ = tx.send(AppEvent::ToolExec(format!("{command}...")));

                    // Interactive permission check via generic Prompt.
                    let mut rejected = false;
                    if !skip_permissions {
                        let needs_confirm =
                            ctx.allowlist.read().await.check(&command).unwrap_or(false);
                        if needs_confirm {
                            let (resp_tx, resp_rx) = oneshot::channel();
                            let _ = tx.send(AppEvent::Prompt(PromptRequest {
                                message: cow_say(
                                    &command,
                                    "[y] approve · [a] allow always · [n] reject",
                                ),
                                hint: "**[y]** approve · **[a]** allow always · **[n]** reject".into(),
                                response_tx: resp_tx,
                            }));
                            let answer = resp_rx.await.unwrap_or(None);
                            match answer.as_deref() {
                                Some("a") | Some("always") => {
                                    ctx.allowlist.write().await.approve(cmd_name);
                                }
                                Some("n") | Some("no") | None => {
                                    rejected = true;
                                }
                                _ => {} // "y" / "yes" → proceed
                            }
                        }
                    }

                    if rejected {
                        let mut s = session.write().await;
                        s.push_message(
                            ChatMessage::tool_result(
                                tc.id.clone(),
                                "execution rejected by user".into(),
                            ),
                            0,
                        );
                        let _ = tx.send(AppEvent::ToolExec("✗ Rejected by user".into()));
                        continue;
                    }

                    // Execute (skip_permissions=true because we already checked).
                    let ai_interactive = tc.arguments.get("interactive").and_then(|v| v.as_bool());
                    let lower = command.to_lowercase();
                    let words: Vec<&str> = lower.split_whitespace().collect();
                    let first = words.first().copied().unwrap_or("");
                    let auto_interactive = first == "sudo" || words.contains(&"sudo");
                    let interactive = ai_interactive.unwrap_or(auto_interactive);
                    let executed = if interactive {
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        let _ = tx.send(AppEvent::InteractiveCommand {
                            command: command.clone(),
                            response_tx: resp_tx,
                        });
                        let msg = resp_rx
                            .await
                            .unwrap_or_else(|_| "interactive: cancelled".to_string());
                        let exit_code = msg
                            .strip_prefix("exit=")
                            .and_then(|s| s.parse::<i32>().ok());
                        let content = format!(
                            "exit_code: {:?}\nstdout:\n(see terminal output above)\n",
                            exit_code
                        );
                        Ok(tools::CommandOutput {
                            stdout: content,
                            stderr: String::new(),
                            exit_code,
                            truncated: false,
                        })
                    } else {
                        let al = ctx.allowlist.read().await;
                        let timeout_s = ctx.storage.config.execution.command_timeout_seconds;
                        let max_bytes = ctx.storage.config.execution.max_output_bytes;
                        let r = tools::execute_command_with_config(
                            &command, &al, true, timeout_s, max_bytes,
                        )
                        .await;
                        drop(al);
                        r
                    };

                    match executed {
                        Ok(output) => {
                            let content = output.to_content();
                            let tokens = active_provider.count_tokens(&content);
                            let mut s = session.write().await;
                            s.push_message(
                                ChatMessage::tool_result(tc.id.clone(), content),
                                tokens,
                            );
                            let _ = tx.send(AppEvent::ToolExec(format!(
                                "{command} → exit={:?}",
                                output.exit_code
                            )));
                        }
                        Err(e) => {
                            let mut s = session.write().await;
                            s.push_message(
                                ChatMessage::tool_result(tc.id.clone(), format!("error: {e}")),
                                0,
                            );
                            let _ = tx.send(AppEvent::ToolExec(format!("{command}: {e}")));
                        }
                    }
                }
                computer::TOOL_NAME => {
                    // Re-read the flag so `/computer off` takes effect even
                    // while this round is still executing.
                    let still_enabled = config
                        .computer_enabled
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::Relaxed));
                    if !still_enabled || !has_tools || !computer::is_supported() {
                        let reason = if !computer::is_supported() {
                            "computer use is not supported on this platform"
                        } else if !still_enabled {
                            "computer use is disabled; use /computer on"
                        } else {
                            "computer use is only available during the tool-calling phase"
                        };
                        let mut s = session.write().await;
                        s.push_message(
                            ChatMessage::tool_result(tc.id.clone(), format!("error: {reason}")),
                            0,
                        );
                        let _ = tx.send(AppEvent::ToolExec(format!("✖ {reason}")));
                        continue;
                    }

                    // Validate before prompting so malformed calls never ask
                    // the user to approve an action that cannot run.
                    if let Err(e) = computer::validate_arguments(&tc.arguments) {
                        let text = computer::error_text(&e);
                        let mut s = session.write().await;
                        s.push_message(ChatMessage::tool_result(tc.id.clone(), text), 0);
                        let _ = tx.send(AppEvent::ToolExec(format!("🖥 invalid: {e}")));
                        continue;
                    }

                    let action = tc
                        .arguments
                        .get("action")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let read_only = matches!(action.as_str(), "screenshot" | "status");
                    let mut approved_by = Approval::Auto;
                    let mut denied = false;

                    if !read_only
                        && config.computer_config.require_confirmation
                        && !config.computer_trusted.load(Ordering::Relaxed)
                    {
                        // A human is now in the loop, whether they approve or
                        // reject; the audit entry should reflect that.
                        approved_by = Approval::Human;
                        let summary = describe_computer_action(&tc.arguments);
                        let (resp_tx, resp_rx) = oneshot::channel();
                        let _ = tx.send(AppEvent::Prompt(PromptRequest {
                            message: cow_say(
                                &format!("🖥 {summary}"),
                                "[y] approve · [a] allow all computer actions this session · [n] reject",
                            ),
                            hint: "**[y]** approve · **[a]** allow all computer actions this session · **[n]** reject".into(),
                            response_tx: resp_tx,
                        }));
                        let timeout = Duration::from_secs(
                            config.computer_config.confirmation_timeout_seconds.max(1),
                        );
                        let answer = match tokio::time::timeout(timeout, resp_rx).await {
                            Ok(Ok(answer)) => answer,
                            _ => None,
                        };
                        match answer.as_deref() {
                            Some("a") | Some("always") => {
                                config.computer_trusted.store(true, Ordering::Relaxed);
                                approved_by = Approval::Human;
                                audit_human_confirm(&ctx, &session, "computer_session_trust", true)
                                    .await;
                            }
                            Some("y") | Some("yes") => {
                                approved_by = Approval::Human;
                            }
                            _ => denied = true,
                        }
                        if denied {
                            let _ = tx.send(AppEvent::PromptExpired);
                        }
                    }

                    let started = Instant::now();
                    if denied {
                        let mut s = session.write().await;
                        s.push_message(
                            ChatMessage::tool_result(
                                tc.id.clone(),
                                "execution rejected by user".into(),
                            ),
                            0,
                        );
                        drop(s);
                        let _ = tx.send(AppEvent::ToolExec("✖ Rejected by user".into()));
                        audit_computer_action(
                            &ctx,
                            &session,
                            &tc.arguments,
                            approved_by,
                            Err("rejected by user".into()),
                            started.elapsed().as_millis() as u64,
                        )
                        .await;
                        continue;
                    }

                    let _ = tx.send(AppEvent::ToolExec(format!(
                        "🖥 {}",
                        describe_computer_action(&tc.arguments)
                    )));

                    match computer_session
                        .execute(&tc.arguments, &config.computer_config)
                        .await
                    {
                        Ok(arcc_core::computer::ComputerOutput {
                            text,
                            images,
                            screen,
                        }) => {
                            let tokens = active_provider.count_tokens(&text);
                            if let Some(image) = images.into_iter().next() {
                                last_screenshot = Some((image, screen));
                            }
                            let mut s = session.write().await;
                            s.push_message(ChatMessage::tool_result(tc.id.clone(), text), tokens);
                            drop(s);
                            audit_computer_action(
                                &ctx,
                                &session,
                                &tc.arguments,
                                approved_by,
                                Ok(()),
                                started.elapsed().as_millis() as u64,
                            )
                            .await;
                            let _ = tx.send(AppEvent::ToolExec(format!("🖥 {action} ✓")));
                        }
                        Err(e) => {
                            let text = computer::error_text(&e);
                            let mut s = session.write().await;
                            s.push_message(ChatMessage::tool_result(tc.id.clone(), text), 0);
                            drop(s);
                            audit_computer_action(
                                &ctx,
                                &session,
                                &tc.arguments,
                                approved_by,
                                Err(e.to_string()),
                                started.elapsed().as_millis() as u64,
                            )
                            .await;
                            let _ = tx.send(AppEvent::ToolExec(format!("🖥 {action}: {e}")));
                        }
                    }
                }
                other => {
                    let mut s = session.write().await;
                    s.push_message(
                        ChatMessage::tool_result(
                            tc.id.clone(),
                            format!("error: unknown tool `{other}`"),
                        ),
                        0,
                    );
                    let _ = tx.send(AppEvent::ToolExec(format!("✖ unknown tool: {other}")));
                }
            }
        }

        // Attach the latest screenshot as a *user* message: DeepSeek only
        // accepts images on user-role messages. Older screenshots are pruned
        // before the next request, so at most one image remains in context.
        if let Some((image, screen)) = last_screenshot {
            let label = match &screen {
                Some(screen) => format!(
                    "[Screenshot] display={} image={}x{}. Coordinates are pixels in this \
                     image, origin top-left.",
                    screen.display, screen.image_width, screen.image_height
                ),
                None => "[Screenshot] Coordinates are pixels in this image, origin top-left."
                    .to_string(),
            };
            let tokens = active_provider.count_tokens(&label) + 1024;
            let mut s = session.write().await;
            s.push_message(ChatMessage::user(label).with_images(vec![image]), tokens);
        }

        // Keep tools available for multi-step computer-use; otherwise retain
        // the existing one-tool-round behavior. Re-read the flag so turning
        // computer-use off ends the multi-round loop immediately.
        let still_enabled = config
            .computer_enabled
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed));
        if still_enabled && round + 1 < config.computer_config.max_rounds.max(1) {
            round += 1;
            phase = 1;
        } else {
            phase = 2;
        }
    }
}

// ---------------------------------------------------------------------------
// Computer-use helpers
// ---------------------------------------------------------------------------

/// Human-readable summary shown in the confirmation prompt.
fn describe_computer_action(args: &serde_json::Value) -> String {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?");
    let display = args.get("display").and_then(|v| v.as_u64()).unwrap_or(0);
    let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
    let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0);

    match action {
        "click" | "double_click" | "right_click" => {
            let button = args
                .get("button")
                .and_then(|v| v.as_str())
                .unwrap_or("left");
            format!("{action} {button} at ({x}, {y}) · display {display}")
        }
        "move" => format!("move to ({x}, {y}) · display {display}"),
        "drag" => {
            let to_x = args.get("to_x").and_then(|v| v.as_i64()).unwrap_or(0);
            let to_y = args.get("to_y").and_then(|v| v.as_i64()).unwrap_or(0);
            format!("drag from ({x}, {y}) to ({to_x}, {to_y}) · display {display}")
        }
        "scroll" => {
            let direction = args
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("down");
            let amount = args.get("amount").and_then(|v| v.as_i64()).unwrap_or(3);
            format!("scroll {direction} {amount} lines")
        }
        "type" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let preview: String = text.chars().take(200).collect();
            let enter = args
                .get("press_enter")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            format!("type \"{preview}\"{}", if enter { " + Enter" } else { "" })
        }
        "key" => {
            let keys = args.get("keys").and_then(|v| v.as_str()).unwrap_or("");
            format!("key {keys}")
        }
        "wait" => {
            let ms = args
                .get("milliseconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(500);
            format!("wait {ms} ms")
        }
        "screenshot" => "screenshot".into(),
        "status" => "status".into(),
        other => other.into(),
    }
}

async fn audit_computer_action(
    ctx: &SharedContext,
    session: &Arc<RwLock<Session>>,
    args: &serde_json::Value,
    approved_by: Approval,
    result: Result<(), String>,
    elapsed_ms: u64,
) {
    let session_id = session.read().await.id.clone();
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let risk = if matches!(action, "screenshot" | "status") {
        RiskLevel::Low
    } else {
        RiskLevel::High
    };
    let result = match result {
        Ok(()) => ExecResult::Ok,
        Err(e) => ExecResult::Error(e),
    };
    // Keep the audit line bounded even if the model typed a long string.
    let mut args_json = args.to_string();
    const MAX_AUDIT_ARGS: usize = 2000;
    if args_json.len() > MAX_AUDIT_ARGS {
        let boundary = args_json.floor_char_boundary(MAX_AUDIT_ARGS);
        args_json.truncate(boundary);
        args_json.push_str("... (truncated)");
    }
    ctx.storage.audit.write(&AuditEvent::ComputerAction {
        ts: Utc::now().to_rfc3339(),
        session: session_id,
        action: action.to_owned(),
        args: args_json,
        risk,
        approved_by,
        result,
        elapsed_ms,
    });
}

async fn audit_human_confirm(
    ctx: &SharedContext,
    session: &Arc<RwLock<Session>>,
    action: &str,
    approved: bool,
) {
    let session_id = session.read().await.id.clone();
    ctx.storage.audit.write(&AuditEvent::HumanConfirm {
        ts: Utc::now().to_rfc3339(),
        session: session_id,
        action: action.to_owned(),
        decision: if approved {
            ConfirmDecision::Approved
        } else {
            ConfirmDecision::Denied
        },
        user: "tui".into(),
    });
}

// ---------------------------------------------------------------------------
// Cow prompt helper — reusable ASCII art for confirmation dialogs
// ---------------------------------------------------------------------------

/// Wrap a confirmation message in a `cowsay`-style bubble.
///
/// The result is markdown (a fenced code block) so the ASCII art keeps its
/// alignment when rendered in the chat area.  The separator width uses the
/// visible display width so CJK command text still lines up.
fn cow_say(title: &str, hint: &str) -> String {
    let sep = "-".repeat(title.width() + 2);
    let cow = format!(
        "< {title} >\n{sep}\n  \\   ^__^\n   \\  (oo)\\_______\n      (__)\\       )\\/\\\n          ||----w |\n          ||     ||"
    );
    if hint.is_empty() {
        format!("```\n{cow}\n```")
    } else {
        format!("```\n{cow}\n{hint}\n```")
    }
}

// ---------------------------------------------------------------------------
// run_project_init — probe project and generate AGENTS.md via AI
// ---------------------------------------------------------------------------

async fn run_project_init(
    ctx: SharedContext,
    tx: mpsc::UnboundedSender<AppEvent>,
    agents_path: &std::path::Path,
    root: &str,
) {
    let _ = tx.send(AppEvent::ToolExec("probing project structure...".into()));
    let mut probe_parts: Vec<String> = Vec::new();

    // Directory tree (top 3 levels, capped at 200 lines).
    let tree_out = tokio::process::Command::new("find")
        .args([
            root,
            "-maxdepth",
            "3",
            "-not",
            "-path",
            "*/target/*",
            "-not",
            "-path",
            "*/.git/*",
            "-not",
            "-path",
            "*/node_modules/*",
        ])
        .output()
        .await
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let tree_lines: Vec<&str> = tree_out.lines().take(200).collect();
    probe_parts.push(format!(
        "## Directory Tree\n```\n{}\n```",
        tree_lines.join("\n")
    ));

    // Key config files.
    for fname in &[
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "README.md",
        "Makefile",
        "Dockerfile",
        ".github/workflows",
    ] {
        let fpath = std::path::Path::new(root).join(fname);
        if fpath.exists()
            && let Ok(content) = std::fs::read_to_string(&fpath)
        {
            let truncated: String = content.chars().take(2000).collect();
            probe_parts.push(format!("\n## {fname}\n```\n{truncated}\n```"));
        }
    }

    // Git log (recent activity).
    let log_out = tokio::process::Command::new("git")
        .args(["-C", root, "log", "--oneline", "-20"])
        .output()
        .await
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    if !log_out.is_empty() {
        probe_parts.push(format!("\n## Recent Git History\n```\n{log_out}\n```"));
    }

    let probe_text = probe_parts.join("\n");

    // Call the AI to generate AGENTS.md.
    let _ = tx.send(AppEvent::Status(components::status::EXECUTING.into()));
    let provider = ctx.providers.pick(&probe_text, false);
    let init_system_content = r#"You are a project analyst. Analyze the project and generate a concise AGENTS.md optimized for AI comprehension. Structure it as follows:

## Project Overview — One-paragraph elevator pitch: what, why, tech stack, target audience.

## Quick Start — The minimal commands to build, test, and run (copy-paste ready).

## Repo Anatomy — Top-level layout explained in 3-5 bullet points: which directory holds what, entry points.

## Intelligent Conventions — Only what matters for code correctness:
- Naming patterns, error handling idioms, async patterns used.
- Git / PR conventions if discernible from history.
- Testing expectations (where tests live, how to run a subset).

## Design Decisions — Non-obvious things a newcomer would get wrong:
- Why certain crates exist, why a particular library was chosen.
- Trade-offs made, workarounds for known issues.
- Configuration / environment variable semantics.

## Common Pitfalls — Frequent gotchas this project has: flaky tests, known bugs, version requirements, OS-specific notes.

## Notice — Key endpoints, flags, or interfaces the AI will likely be asked about.

Write ONLY the AGENTS.md content, no extra commentary, no section if it would be empty. Use clean markdown. Be specific — prefer concrete file paths, commands, and examples over generic advice."#;

    let mut full_content = String::new();
    let _ = tx.send(AppEvent::Status(components::status::EXECUTING.into()));

    if let Some(prov) = provider {
        let init_system = arcc_core::model::types::ChatMessage::system(init_system_content.into());
        let user_msg = arcc_core::model::types::ChatMessage::user(format!(
            "Generate an AGENTS.md for this project:\n\n{probe_text}"
        ));
        let req = arcc_core::model::types::ChatRequest {
            model: prov.model_name().to_owned(),
            messages: vec![init_system, user_msg],
            tools: None,
            tool_choice: None,
            temperature: Some(0.5),
            max_tokens: Some(4096),
            stream: true,
            thinking_mode: None,
            reasoning_effort: None,
        };
        match prov.chat_stream(req).await {
            Ok(stream) => {
                use futures::StreamExt;
                let mut stream = Box::pin(stream);
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(arcc_core::model::types::StreamChunk::Content(text)) => {
                            full_content.push_str(&text);
                            let _ = tx.send(AppEvent::Token(text));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            let _ = tx.send(AppEvent::Token(format!("\n⚠ {e}")));
                        }
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(AppEvent::Token(format!("\n❌ {e}")));
                let _ = tx.send(AppEvent::StreamDone);
                return;
            }
        }
    } else {
        let _ = tx.send(AppEvent::Token("\n❌ No model provider available.".into()));
        let _ = tx.send(AppEvent::StreamDone);
        return;
    }

    // Strip ``` fences if AI wrapped the content.
    let clean = full_content
        .trim()
        .strip_prefix("```markdown")
        .or_else(|| full_content.trim().strip_prefix("```md"))
        .or_else(|| full_content.trim().strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(full_content.trim());

    if let Err(e) = std::fs::write(agents_path, clean) {
        let _ = tx.send(AppEvent::Token(format!(
            "\n⚠ Failed to write AGENTS.md: {e}"
        )));
    } else {
        let _ = tx.send(AppEvent::Token(format!(
            "\n✅ AGENTS.md created at `{}`",
            agents_path.display()
        )));
        if let Ok(content) = std::fs::read_to_string(agents_path) {
            *ctx.project_instructions.write().await = Some(content);
        }
    }
    let _ = tx.send(AppEvent::StreamDone);
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
    execute!(stdout, EnableMouseCapture)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    let (tx, rx) = crate::event::loop_event::create_event_loop();
    let mut input_handle = crate::event::handler::spawn_input_handler(tx.clone());
    let mut app = App::new(tx.clone(), rx, ctx);

    while app.running {
        while let Ok(event) = app.event_rx.try_recv() {
            match event {
                AppEvent::Input(ch) if ch == "\n" || ch == "\r" => {
                    // If a tree block is focused and input is empty, cycle its view mode.
                    if app.input_buffer.trim().is_empty()
                        && let Some(hash) = app.focused_tree
                    {
                        let mut reg = app.tree_registry.lock().unwrap();
                        if let Some(entry) = reg.get_mut(&hash) {
                            entry.mode = match entry.mode {
                                components::TreeViewMode::Collapsed => {
                                    components::TreeViewMode::Expanded
                                }
                                components::TreeViewMode::Expanded => components::TreeViewMode::Raw,
                                components::TreeViewMode::Raw => {
                                    components::TreeViewMode::Collapsed
                                }
                            };
                        }
                        app.input_buffer.clear();
                        app.character_index = 0;
                        continue;
                    }
                    // Check for pending generic prompt.
                    if let Some(prompt) = app.pending_prompt.take() {
                        let input = app.input_buffer.trim().to_lowercase();
                        app.input_buffer.clear();
                        app.character_index = 0;
                        app.status = components::status::IDLE.into();
                        let response = if input.is_empty() { None } else { Some(input) };
                        let _ = prompt.response_tx.send(response);
                        continue;
                    }
                    if app.show_dashboard && app.input_buffer.trim().is_empty() {
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
                                    app.messages.push(format!(
                                        "🤖 **Session `{short_id}` — last {} messages**",
                                        msgs.len()
                                    ));
                                    for m in msgs.iter().rev() {
                                        let (role_emoji, _role_label) = match m.role.as_str() {
                                            "user" => ("🧑 ", "user"),
                                            "assistant" => ("🤖 ", "assistant"),
                                            "tool" => ("⚡ ", "tool"),
                                            _ => ("📝 ", "?"),
                                        };
                                        app.messages.push(format!("{role_emoji}{}", m.content));
                                        let ts = m.created_at.as_deref().unwrap_or("?");
                                        let tokens = m
                                            .token_count
                                            .map(|t| format!(" ({t} tok)"))
                                            .unwrap_or_default();
                                        app.messages.push(format!("  _· {ts}{tokens}_"));
                                    }
                                    app.messages
                                        .push("🤖 Press Esc to return to dashboard".into());
                                } else {
                                    app.messages
                                        .push("⚠ Failed to load session messages".into());
                                }
                                app.input_buffer.clear();
                                app.character_index = 0;
                            }
                        }
                    } else if app.status == components::status::IDLE {
                        app.submit(tx.clone());
                    }
                    // Non-IDLE: Enter silently ignored — user can type their
                    // next message while AI responds, and submit when finished.
                }
                AppEvent::InteractiveCommand {
                    command,
                    response_tx,
                } => {
                    // Pause the crossterm input handler so it doesn't compete
                    // with the child process for stdin.
                    input_handle.abort();
                    let _ = input_handle.await;

                    // Exit alternate screen + raw mode → child output goes to
                    // the primary screen, cleanly separated from the TUI.
                    execute!(terminal.backend_mut(), DisableMouseCapture)?;
                    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                    disable_raw_mode()?;

                    let (shell, arg) = arcc_core::system::shell_and_arg();
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

                    // Re-enter TUI mode.  Only restore mouse capture if the
                    // user hasn't turned it off via `/mouse off`.
                    enable_raw_mode()?;
                    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                    if app.mouse_capture {
                        execute!(terminal.backend_mut(), EnableMouseCapture)?;
                    }
                    terminal.clear()?;

                    // Re-spawn the input handler now that the child is done.
                    input_handle = crate::event::handler::spawn_input_handler(tx.clone());
                    let _ = response_tx.send(format!("exit={exit_code}"));
                }
                AppEvent::SetMouseCapture(enabled) => {
                    app.mouse_capture = enabled;
                    if enabled {
                        execute!(terminal.backend_mut(), EnableMouseCapture)?;
                    } else {
                        execute!(terminal.backend_mut(), DisableMouseCapture)?;
                    }
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

            let phase = if app.status != components::status::IDLE {
                app.blink
            } else {
                99
            };
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
                let model = app
                    .ctx
                    .providers
                    .flash()
                    .map(|p| p.model_name())
                    .unwrap_or("?");
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
            components::render_input(f, areas[4], &app.input_buffer, &app.status);
            components::render_divider(f, areas[5]);

            // Calculate visual cursor position using Unicode width (Chinese chars = 2 columns).
            let prefix = app.input_buffer[..app.byte_index()].to_string();
            let visual_width = prefix.as_str().width();
            #[expect(clippy::cast_possible_truncation)]
            let cursor_x = areas[4].x + 2 + visual_width as u16;
            f.set_cursor_position(Position::new(cursor_x, areas[4].y));
        })?;

        // Prune oldest messages if the buffer exceeds the cap.
        if app.messages.len() > MAX_STORED_MSGS {
            let excess = app.messages.len() - MAX_STORED_MSGS;
            let _ = app.messages.drain(..excess);
        }

        app.tick = app.tick.wrapping_add(1);
        tokio::time::sleep(std::time::Duration::from_millis(16)).await;
    }

    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    execute!(terminal.backend_mut(), DisableBracketedPaste)?;
    disable_raw_mode()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cow_say;

    #[test]
    fn cow_say_aligns_separator_with_display_width() {
        let art = cow_say("打开豆包", "[y] yes · [n] no");
        let lines: Vec<&str> = art.lines().collect();

        // ``` / < title > / separator / cow art / hint / ```
        assert_eq!(lines[0], "```");
        assert_eq!(lines[1], "< 打开豆包 >");
        // 4 CJK chars = 8 display columns, plus the two padding spaces.
        assert_eq!(lines[2], "-".repeat(10));
        assert!(lines.iter().any(|line| line.contains("(oo)")));
        assert!(lines.iter().any(|line| line.contains("[y] yes")));
    }

    #[test]
    fn cow_say_omits_hint_when_empty() {
        let art = cow_say("Overwrite AGENTS.md?", "");
        assert!(!art.contains("[y]"));
        assert!(art.contains("(oo)"));
    }
}
