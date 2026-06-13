use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::model::provider::ModelProvider;
use crate::model::types::{ChatMessage, ChatRequest};

/// Manages a single conversation session: message history, token budget,
/// and automatic context compression.
pub struct Session {
    pub id: String,
    pub name: String,
    pub mode: String,
    messages: VecDeque<ChatMessage>,
    token_count: usize,
    context_max_tokens: usize,
    summary: Option<String>,
    /// Shared database connection for automatic persistence.
    /// When `Some`, every `push_message` call also writes to SQLite.
    db: Option<Arc<Mutex<rusqlite::Connection>>>,
}

impl Session {
    pub fn new(
        id: &str,
        name: &str,
        mode: &str,
        context_max_tokens: usize,
        db: Option<Arc<Mutex<rusqlite::Connection>>>,
    ) -> Self {
        let session = Self {
            id: id.to_owned(),
            name: name.to_owned(),
            mode: mode.to_owned(),
            messages: VecDeque::new(),
            token_count: 0,
            context_max_tokens,
            summary: None,
            db,
        };
        session.persist_new_session();
        session
    }

    /// Write the new session row to SQLite (best-effort, logs on failure).
    fn persist_new_session(&self) {
        let db = match self.db {
            Some(ref db) => db,
            None => return,
        };
        match db.lock() {
            Ok(conn) => {
                if let Err(e) = conn.execute(
                    "INSERT INTO sessions (id, name, mode, created_at, last_active_at) \
                     VALUES (?1, ?2, ?3, datetime('now'), datetime('now')) \
                     ON CONFLICT(id) DO UPDATE SET last_active_at = datetime('now')",
                    rusqlite::params![self.id, self.name, self.mode],
                ) {
                    warn!(err = %e, session = %self.id, "failed to persist session");
                }
            }
            Err(e) => {
                warn!(err = %e, "db lock poisoned");
            }
        }
    }

    /// Persist the current summary to the `sessions` table (best-effort).
    fn persist_summary(&self) {
        let db = match self.db {
            Some(ref db) => db,
            None => return,
        };
        let summary = match self.summary {
            Some(ref s) => s,
            None => return,
        };
        match db.lock() {
            Ok(conn) => {
                if let Err(e) = conn.execute(
                    "UPDATE sessions SET summary = ?2 WHERE id = ?1",
                    rusqlite::params![self.id, summary],
                ) {
                    warn!(err = %e, session = %self.id, "failed to persist summary");
                }
            }
            Err(e) => {
                warn!(err = %e, "db lock poisoned");
            }
        }
    }

    /// Add a message to the session history and persist to SQLite.
    pub fn push_message(&mut self, msg: ChatMessage, tokens: usize) {
        self.token_count += tokens;
        self.messages.push_back(msg);
        self.persist_message(tokens);
    }

    /// Persist the most recent message to SQLite (best-effort, logs on failure).
    fn persist_message(&self, tokens: usize) {
        let db = match self.db {
            Some(ref db) => db,
            None => return,
        };
        let msg = match self.messages.back() {
            Some(m) => m,
            None => return,
        };
        match db.lock() {
            Ok(conn) => {
                if let Err(e) = conn.execute(
                    "INSERT INTO messages (session_id, role, content, token_count, created_at) \
                     VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                    rusqlite::params![self.id, msg.role, msg.content, tokens as i64],
                ) {
                    warn!(err = %e, session = %self.id, "failed to persist message");
                }
                // Bump session last-active timestamp.
                if let Err(e) = conn.execute(
                    "UPDATE sessions SET last_active_at = datetime('now') WHERE id = ?1",
                    rusqlite::params![self.id],
                ) {
                    warn!(err = %e, session = %self.id, "failed to update session active time");
                }
            }
            Err(e) => {
                warn!(err = %e, "db lock poisoned");
            }
        }
    }

    /// Return current messages as a `Vec`, prepending summary if present.
    pub fn context(&self) -> Vec<ChatMessage> {
        let mut msgs = Vec::with_capacity(self.messages.len() + 1);
        if let Some(ref summary) = self.summary {
            msgs.push(ChatMessage {
                role: "system".into(),
                content: format!("[conversation summary] {summary}"),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        }
        msgs.extend(self.messages.iter().cloned());
        msgs
    }

    /// Check if the context is approaching the token limit.
    pub fn needs_compression(&self) -> bool {
        self.token_count > self.context_max_tokens
    }

    /// Compress the conversation: send history to the flash model to
    /// produce a concise summary, then replace old messages with it.
    pub async fn compress(&mut self, flash_provider: &dyn ModelProvider) {
        if self.messages.is_empty() {
            return;
        }

        let old_count = self.messages.len();
        let old_tokens = self.token_count;

        // Build summarisation prompt.
        let history_text: String = self
            .messages
            .iter()
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");

        let compress_prompt = ChatRequest {
            model: flash_provider.model_name().to_owned(),
            messages: vec![
                crate::model::prompts::templates::compress().to_chat_message(),
                ChatMessage {
                    role: "user".into(),
                    content: format!("Summarise this conversation:\n{history_text}"),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            tools: None,
            tool_choice: None,
            temperature: Some(0.3),
            max_tokens: Some(1024),
            stream: false,
            thinking_mode: None,
            reasoning_effort: None,
        };

        match flash_provider.chat(compress_prompt).await {
            Ok(resp) => {
                let summary_text = resp.message.content;
                info!(
                    session = %self.id,
                    old_msgs = old_count,
                    old_tokens,
                    summary_len = summary_text.len(),
                    "context compressed"
                );

                // Replace messages with summary.
                self.messages.clear();
                self.messages.push_back(ChatMessage {
                    role: "assistant".into(),
                    content: format!("[summary] {summary_text}"),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
                self.summary = Some(summary_text);
                self.persist_summary();
                self.token_count = flash_provider.count_tokens(
                    &self.messages[0].content
                );
            }
            Err(e) => {
                warn!(
                    session = %self.id,
                    err = %e,
                    "context compression failed — truncating instead"
                );
                // Fallback: drop oldest messages.
                while self.token_count > self.context_max_tokens && self.messages.len() > 2 {
                    if let Some(msg) = self.messages.pop_front() {
                        self.token_count = self.token_count.saturating_sub(
                            flash_provider.count_tokens(&msg.content),
                        );
                    }
                }
            }
        }
    }

    /// Clear all in-memory messages and reset token count.
    ///
    /// Used by TUI `/clear` to reset the conversation without dropping
    /// the session or its DB records.
    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.token_count = 0;
        self.summary = None;
    }

    /// Lazily enable DB persistence for a session that was created with
    /// `db: None`.  Writes the session row to SQLite and retroactively
    /// persists any messages already in the VecDeque.
    ///
    /// Used by TUI to defer DB writes until the first LLM content chunk
    /// arrives, avoiding orphaned empty sessions.
    pub fn enable_persistence(&mut self, db: Arc<Mutex<rusqlite::Connection>>) {
        if self.db.is_some() {
            return;
        }
        self.db = Some(db);
        self.persist_new_session();

        // Retroactively persist messages that were pushed before persistence
        // was enabled.  Token counts are not available at this point, so we
        // store NULL — they are still tracked in the in-memory counter.
        if let Some(ref db_lock) = self.db && let Ok(conn) = db_lock.lock() {
            if let Ok(mut stmt) = conn.prepare(
                "INSERT INTO messages (session_id, role, content, token_count, created_at)
                 VALUES (?1, ?2, ?3, NULL, datetime('now'))",
            ) {
                for msg in &self.messages {
                    let _ = stmt.execute(rusqlite::params![self.id, msg.role, msg.content]);
                }
            }
            // Bump session last-active timestamp.
            let _ = conn.execute(
                "UPDATE sessions SET last_active_at = datetime('now') WHERE id = ?1",
                rusqlite::params![self.id],
            );
        }
    }

    /// Prepare messages for an API request, sanitizing `reasoning_content` on
    /// assistant tool-call messages when `thinking_mode` is enabled.
    ///
    /// DeepSeek V4's thinking-mode API requires every assistant message carrying
    /// `tool_calls` to also include a non-empty `reasoning_content` field on
    /// subsequent turns — omitting it triggers HTTP 400.
    ///
    /// This method:
    /// 1. Returns `context()` (messages + optional summary).
    /// 2. If `thinking_mode = true`, injects a `"(reasoning omitted)"` placeholder
    ///    into any assistant tool-call message whose `reasoning_content` is missing
    ///    or empty.  This mirrors CodeWhale's `sanitize_thinking_mode_messages()`.
    ///
    /// The stored session messages are **not** modified — placeholders exist only
    /// in the returned `Vec` for the wire format.
    pub fn prepare_for_request(&self, thinking_mode: bool) -> Vec<ChatMessage> {
        let mut msgs = self.context();
        if !thinking_mode {
            return msgs;
        }
        for msg in &mut msgs {
            if msg.role != "assistant" {
                continue;
            }
            let has_tool_calls = msg.tool_calls.as_ref().is_some_and(|c| !c.is_empty());
            if !has_tool_calls {
                continue;
            }
            let needs_placeholder = msg
                .reasoning_content
                .as_ref()
                .is_none_or(|s| s.trim().is_empty());
            if needs_placeholder {
                msg.reasoning_content = Some("(reasoning omitted)".into());
            }
        }
        msgs
    }

    /// Attach accumulated reasoning content to the most recent assistant message.
    ///
    /// Call this after streaming completes for a turn where `reasoning_content`
    /// was received alongside (or before) the final content / tool calls.
    /// DeepSeek streams `reasoning_content` deltas independently of `content`
    /// deltas, so the reasoning may finish arriving after the assistant message
    /// has already been pushed during the tool-call execution loop.
    pub fn attach_reasoning(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(msg) = self.messages.iter_mut().rev().find(|m| m.role == "assistant") {
            let existing = msg.reasoning_content.get_or_insert(String::new());
            // Only append if the reasoning wasn't already set (e.g. during
            // the tool-call loop we may have pushed it up to that point).
            if existing.trim().is_empty() || existing == "(reasoning omitted)" {
                *existing = text.to_owned();
            }
        }
    }

    /// Total token count for the current context window.
    pub fn token_count(&self) -> usize {
        self.token_count
    }

    /// Number of messages in the session.
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}

/// Manages all active sessions, keyed by session ID.
pub struct SessionManager {
    sessions: RwLock<HashMap<String, Arc<RwLock<Session>>>>,
    context_max_tokens: usize,
    db: Option<Arc<Mutex<rusqlite::Connection>>>,
}

impl SessionManager {
    pub fn new(context_max_tokens: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            context_max_tokens,
            db: None,
        }
    }

    /// Create a `SessionManager` with a shared database connection for persistence.
    pub fn with_db(context_max_tokens: usize, db: Arc<Mutex<rusqlite::Connection>>) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            context_max_tokens,
            db: Some(db),
        }
    }

    /// Set (or replace) the database connection.
    pub fn set_db(&mut self, db: Arc<Mutex<rusqlite::Connection>>) {
        self.db = Some(db);
    }

    /// Create a new session and return it.
    pub async fn create(&self, name: &str, mode: &str) -> Arc<RwLock<Session>> {
        let id = uuid::Uuid::new_v4().to_string();
        let session = Arc::new(RwLock::new(Session::new(
            &id,
            name,
            mode,
            self.context_max_tokens,
            self.db.clone(),
        )));
        self.sessions.write().await.insert(id.clone(), Arc::clone(&session));
        info!(%id, name, mode, "session created");
        session
    }

    /// Find a session by ID — O(1) lookup.
    pub async fn find(&self, id: &str) -> Option<Arc<RwLock<Session>>> {
        self.sessions.read().await.get(id).cloned()
    }

    /// Find a session by its name (not ID).
    ///
    /// Iterates over all active sessions to find one whose `name` matches.
    /// This is an O(n) lookup — use sparingly.
    pub async fn find_by_name(&self, name: &str) -> Option<Arc<RwLock<Session>>> {
        let map = self.sessions.read().await;
        map.values().find(|s| {
            s.try_read().ok().is_some_and(|guard| guard.name == name)
        }).cloned()
    }

    /// Get an existing session by name, or create a new one if none exists.
    ///
    /// This enables continuous conversations keyed by a stable identifier
    /// (e.g. feishu chat_id) instead of always starting fresh.
    pub async fn get_or_create(&self, name: &str, mode: &str) -> Arc<RwLock<Session>> {
        if let Some(session) = self.find_by_name(name).await {
            debug!(%name, mode, "reusing existing session");
            return session;
        }
        self.create(name, mode).await
    }

    /// Remove a session by ID.
    pub async fn remove(&self, id: &str) {
        self.sessions.write().await.remove(id);
        info!(%id, "session removed");
    }

    /// Number of active sessions.
    pub async fn count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Check all sessions and compress any that exceed the token limit.
    pub async fn compress_all(&self, flash_provider: &dyn ModelProvider) {
        // Clone Arcs under the read lock so we don't hold it during async calls.
        let sessions: Vec<_> = {
            let map = self.sessions.read().await;
            map.values().cloned().collect()
        };
        for s in &sessions {
            let mut session = s.write().await;
            if session.needs_compression() {
                debug!(session = %session.id, tokens = session.token_count(), "compressing session");
                session.compress(flash_provider).await;
            }
        }
    }
}
