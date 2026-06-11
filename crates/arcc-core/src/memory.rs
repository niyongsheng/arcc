//! Persistent memory system for server mode.
//!
//! [`MemoryManager`] extracts key-value facts from user-assistant exchanges
//! and stores them in SQLite for injection into future conversations.

use std::sync::{Arc, Mutex};

use arcc_storage::db::models::MemoryFact;
use tracing::warn;

use crate::model::prompts::templates;
use crate::model::provider::ModelError;
use crate::model::types::{ChatMessage, ChatRequest};

/// Manages persistent memory facts for the server mode.
///
/// Facts are stored as `(user_id, key, value)` triples in the `memories` table.
/// Each fact has a `source` field (`"extraction"` for auto-extracted facts,
/// `"manual"` for API-created facts).
#[derive(Clone)]
pub struct MemoryManager {
    db: Arc<Mutex<rusqlite::Connection>>,
    flash_provider: Arc<dyn crate::model::provider::ModelProvider>,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("model provider error: {0}")]
    Model(#[from] ModelError),
}

impl MemoryManager {
    /// Create a new `MemoryManager`.
    pub fn new(
        db: Arc<Mutex<rusqlite::Connection>>,
        flash_provider: Arc<dyn crate::model::provider::ModelProvider>,
    ) -> Self {
        Self { db, flash_provider }
    }

    /// Extract facts from a user-assistant exchange and store them.
    ///
    /// This calls the flash provider with a dedicated extraction prompt and
    /// parses the response as `key: value` lines or `NO_NEW_FACTS`.
    /// Errors are logged as warnings and silently swallowed — memory extraction
    /// is a best-effort background operation.
    pub async fn extract(
        &self,
        user_id: &str,
        user_msg: &str,
        asst_msg: &str,
    ) -> Result<(), MemoryError> {
        let exchange = format!("User: {user_msg}\n\nAssistant: {asst_msg}");
        let req = ChatRequest {
            model: self.flash_provider.model_name().to_owned(),
            messages: vec![
                templates::memory_extract().to_chat_message(),
                ChatMessage {
                    role: "user".into(),
                    content: exchange,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            tools: None,
            tool_choice: None,
            temperature: Some(0.1),
            max_tokens: Some(256),
            stream: false,
            thinking_mode: None,
            reasoning_effort: None,
        };

        let resp = match self.flash_provider.chat(req).await {
            Ok(r) => r,
            Err(e) => {
                warn!(err = %e, "memory extraction LLM call failed");
                return Err(MemoryError::Model(e));
            }
        };

        let body = resp.message.content.trim().to_owned();
        if body == "NO_NEW_FACTS" || body.is_empty() {
            return Ok(());
        }

        let conn = self.db.lock().expect("db mutex poisoned");

        for line in body.lines() {
            let line = line.trim();
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_lowercase().replace(' ', "-");
                let value = value.trim();
                if key.is_empty() || value.is_empty() {
                    continue;
                }
                if let Err(e) = conn.execute(
                    "INSERT INTO memories (user_id, key, value, source, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'extraction', datetime('now'), datetime('now'))
                     ON CONFLICT(user_id, key) DO UPDATE SET
                         value = ?3, source = 'extraction', updated_at = datetime('now')",
                    rusqlite::params![user_id, key, value],
                ) {
                    warn!(err = %e, "failed to persist memory fact: {key}");
                }
            }
        }

        Ok(())
    }

    /// Format all known facts for a user into a context string suitable for
    /// injection as a system message.
    ///
    /// Returns an empty string if there are no facts.
    pub fn format_for_context(&self, user_id: &str) -> String {
        let facts = match self.list(user_id) {
            Ok(f) => f,
            Err(e) => {
                warn!(err = %e, "failed to list memories for context");
                return String::new();
            }
        };

        if facts.is_empty() {
            return String::new();
        }

        let mut out = String::from("## Known Facts\n\nThe following facts are known about you:\n\n");
        for fact in &facts {
            out.push_str(&format!("- {}: {}\n", fact.key, fact.value));
        }
        out.push_str("\nThese facts were learned from previous conversations.");
        out
    }

    // ── CRUD operations (delegate to ArccStorage queries) ────────────

    /// List all memory facts for a user.
    pub fn list(&self, user_id: &str) -> Result<Vec<MemoryFact>, MemoryError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, user_id, key, value, source, created_at, updated_at
             FROM memories
             WHERE user_id = ?1
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![user_id], |row| {
            Ok(MemoryFact {
                id: Some(row.get(0)?),
                user_id: row.get(1)?,
                key: row.get(2)?,
                value: row.get(3)?,
                source: row.get(4)?,
                created_at: Some(row.get(5)?),
                updated_at: Some(row.get(6)?),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(MemoryError::Db)
    }

    /// Insert or update a memory fact.
    pub fn set(
        &self,
        user_id: &str,
        key: &str,
        value: &str,
        source: &str,
    ) -> Result<(), MemoryError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT INTO memories (user_id, key, value, source, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'), datetime('now'))
             ON CONFLICT(user_id, key) DO UPDATE SET
                 value = ?3, source = ?4, updated_at = datetime('now')",
            rusqlite::params![user_id, key, value, source],
        )?;
        Ok(())
    }

    /// Delete a single memory fact. Returns `true` if a row was deleted.
    pub fn delete(&self, user_id: &str, key: &str) -> Result<bool, MemoryError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        let n = conn.execute(
            "DELETE FROM memories WHERE user_id = ?1 AND key = ?2",
            rusqlite::params![user_id, key],
        )?;
        Ok(n > 0)
    }

    /// Delete all memory facts for a user. Returns number of rows deleted.
    pub fn clear(&self, user_id: &str) -> Result<usize, MemoryError> {
        let conn = self.db.lock().expect("db mutex poisoned");
        let n = conn.execute(
            "DELETE FROM memories WHERE user_id = ?1",
            rusqlite::params![user_id],
        )?;
        Ok(n)
    }
}
