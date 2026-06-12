pub mod audit;
pub mod config;
pub mod db;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::info;

use crate::audit::types::AuditEvent;
use crate::db::queries;
use crate::db::models::{InputHistoryEntry, MemoryFact, Message, ScheduledTask, Session, Summary};

/// Top-level initialisation: loads config, opens DB, runs migrations.
///
/// The database connection is wrapped in `Arc<Mutex>` so it can be shared
/// across threads and subsystems.
pub struct ArccStorage {
    pub config: config::loader::ArccConfig,
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub audit: audit::writer::AuditWriter,
    /// Path to the JSONL audit file, stored so we can re-open it for reads.
    pub audit_path: PathBuf,
}

impl ArccStorage {
    /// Bootstrap the storage layer from the ARCC home directory.
    pub fn init(home: &Path) -> Result<Self, StorageError> {
        std::fs::create_dir_all(home).ok();

        let config_path = home.join("config.toml");
        let config = if config_path.exists() {
            config::loader::load(&config_path)?
        } else {
            info!("no config.toml found, using defaults");
            config::loader::ArccConfig::default()
        };

        let db_path = home.join(&config.storage.db_path);
        let db = Arc::new(Mutex::new(
            db::init(&db_path).map_err(StorageError::Db)?,
        ));

        let audit_path = home.join(&config.logging.log_dir).join("audit.jsonl");
        let audit = audit::writer::AuditWriter::open(&audit_path).map_err(StorageError::Io)?;

        info!("storage layer initialised");
        Ok(Self {
            config,
            db,
            audit,
            audit_path,
        })
    }

    // ── Query helpers for `/data` command ──────────────────────────────

    /// List the most recent sessions.
    pub fn list_sessions(&self, limit: usize) -> Result<Vec<Session>, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::list_sessions(&conn, limit)?)
    }

    /// Get messages for a session.
    pub fn session_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<Message>, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::session_messages(&conn, session_id, limit)?)
    }

    /// Token usage aggregated by day and model, for the last N days.
    pub fn token_usage_daily(
        &self,
        days: usize,
    ) -> Result<Vec<queries::TokenUsageRow>, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::token_usage_daily(&conn, days)?)
    }

    /// Total token counts for the last N days.
    pub fn total_tokens(&self, days: usize) -> Result<(i64, i64), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::total_tokens(&conn, days)?)
    }

    /// Count total messages across all sessions.
    pub fn message_count(&self) -> Result<i64, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::count_messages(&conn)?)
    }

    /// Record (or accumulate) token usage for a session + model on today's date.
    pub fn record_token_usage(
        &self,
        session_id: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<(), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::upsert_token_usage(&conn, session_id, model, input_tokens, output_tokens)?)
    }

    /// Latest summary for a session.
    pub fn latest_summary(&self, session_id: &str) -> Result<Option<Summary>, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::latest_summary(&conn, session_id)?)
    }

    /// Read the most recent N audit events.
    pub fn recent_audit(&self, count: usize) -> Result<Vec<AuditEvent>, StorageError> {
        Ok(audit::reader::read_recent(&self.audit_path, count)?)
    }

    /// Update the summary text stored directly on a session row.
    pub fn update_session_summary(
        &self,
        session_id: &str,
        summary: &str,
    ) -> Result<(), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::update_session_summary(&conn, session_id, summary)?)
    }

    /// Record a user prompt in the input history.
    pub fn record_input_history(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<(), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::insert_input_history(&conn, session_id, prompt)?)
    }

    /// List the most recent N input history entries.
    pub fn recent_input_history(&self, limit: usize) -> Result<Vec<InputHistoryEntry>, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::list_input_history(&conn, limit)?)
    }

    // ── Memory (memories table) ─────────────────────────────────────

    /// List all memory facts for a user.
    pub fn list_memories(&self, user_id: &str) -> Result<Vec<MemoryFact>, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::list_memories(&conn, user_id)?)
    }

    /// Insert or update a memory fact.
    pub fn upsert_memory(
        &self,
        user_id: &str,
        key: &str,
        value: &str,
        source: &str,
    ) -> Result<(), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::upsert_memory(&conn, user_id, key, value, source)?)
    }

    /// Delete a single memory fact. Returns `true` if deleted.
    pub fn delete_memory(&self, user_id: &str, key: &str) -> Result<bool, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::delete_memory(&conn, user_id, key)?)
    }

    /// Delete all memory facts for a user. Returns number of rows deleted.
    pub fn clear_memories(&self, user_id: &str) -> Result<usize, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::clear_memories(&conn, user_id)?)
    }

    // ── Scheduled tasks ──────────────────────────────────

    /// Create a new scheduled task.
    pub fn create_scheduled_task(&self, task: &ScheduledTask) -> Result<(), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::create_scheduled_task(&conn, task)?)
    }

    /// List all pending tasks whose `next_run_at` has passed.
    pub fn list_due_tasks(&self) -> Result<Vec<ScheduledTask>, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::list_due_tasks(&conn)?)
    }

    /// Update the status of a scheduled task.
    pub fn update_task_status(&self, id: &str, status: &str) -> Result<(), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::update_task_status(&conn, id, status)?)
    }

    /// Update the next_run_at timestamp for a recurring task.
    pub fn update_task_next_run(&self, id: &str, next_run_at: &str) -> Result<(), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::update_task_next_run(&conn, id, next_run_at)?)
    }

    /// List active scheduled tasks for a chat_id.
    pub fn list_tasks_by_user(&self, chat_id: &str) -> Result<Vec<ScheduledTask>, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::list_tasks_by_user(&conn, chat_id)?)
    }

    /// Delete a scheduled task. Returns `true` if deleted.
    pub fn delete_task(&self, id: &str) -> Result<bool, StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::delete_task(&conn, id)?)
    }

    /// Pause a scheduled task (scheduler will skip it).
    pub fn pause_task(&self, id: &str) -> Result<(), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::pause_task(&conn, id)?)
    }

    /// Resume a paused task.
    pub fn resume_task(&self, id: &str) -> Result<(), StorageError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        Ok(queries::resume_task(&conn, id)?)
    }

    /// Shortcut: init from the default home (`~/.arcc/`).
    pub fn init_default() -> Result<Self, StorageError> {
        let home = home_dir();
        Self::init(&home)
    }
}

fn home_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ARCC_HOME") {
        return PathBuf::from(dir);
    }
    let base = dirs_fallback();
    base.join(".arcc")
}

fn dirs_fallback() -> PathBuf {
    // macOS / Linux: $HOME, Windows: %USERPROFILE%
    if let Ok(home) = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
    {
        return PathBuf::from(home);
    }
    PathBuf::from(".")
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("config error: {0}")]
    Config(#[from] config::loader::ConfigError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
