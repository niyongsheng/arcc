//! Query helpers for inspecting persisted data in the TUI `/data` command.
//!
//! All functions take a `&rusqlite::Connection` and are meant to be called
//! from `ArccStorage` methods which manage the mutex lock.

use rusqlite::{params, Connection, Result};

use super::models::{InputHistoryEntry, MemoryFact, Message, Session, Summary};

/// Row returned by `token_usage_daily`.
#[derive(Debug, Clone)]
pub struct TokenUsageRow {
    pub date: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// List the most recent sessions, ordered by `last_active_at DESC`.
pub fn list_sessions(conn: &Connection, limit: usize) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, mode, created_at, last_active_at, summary
         FROM sessions
         ORDER BY last_active_at DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(Session {
            id: row.get(0)?,
            name: row.get(1)?,
            mode: row.get(2)?,
            created_at: row.get(3)?,
            last_active_at: row.get(4)?,
            summary: row.get(5)?,
        })
    })?;
    rows.collect()
}

/// Get messages for a session, newest first.
///
/// Uses `LIMIT ?1` — pass 0 for no limit (returns all).
pub fn session_messages(conn: &Connection, session_id: &str, limit: usize) -> Result<Vec<Message>> {
    let mut stmt = if limit > 0 {
        conn.prepare(
            "SELECT id, session_id, role, content, token_count, created_at
             FROM messages
             WHERE session_id = ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?
    } else {
        conn.prepare(
            "SELECT id, session_id, role, content, token_count, created_at
             FROM messages
             WHERE session_id = ?1
             ORDER BY id ASC",
        )?
    };

    let rows = if limit > 0 {
        stmt.query_map(params![session_id, limit as i64], map_message)?
    } else {
        stmt.query_map(params![session_id], map_message)?
    };
    rows.collect()
}

fn map_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: Some(row.get(0)?),
        session_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        token_count: row.get(4)?,
        created_at: Some(row.get(5)?),
    })
}

/// Get token usage aggregated by day and model, for the last N days.
pub fn token_usage_daily(conn: &Connection, days: usize) -> Result<Vec<TokenUsageRow>> {
    let mut stmt = conn.prepare(
        "SELECT date, model, SUM(input_tokens), SUM(output_tokens)
         FROM token_usage
         WHERE date >= date('now', ?1)
         GROUP BY date, model
         ORDER BY date DESC, model",
    )?;
    let offset = format!("-{} days", days);
    let rows = stmt.query_map(params![offset], |row| {
        Ok(TokenUsageRow {
            date: row.get(0)?,
            model: row.get(1)?,
            input_tokens: row.get(2)?,
            output_tokens: row.get(3)?,
        })
    })?;
    rows.collect()
}

/// Get the latest summary for a session, if any.
pub fn latest_summary(conn: &Connection, session_id: &str) -> Result<Option<Summary>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, range_start_msg_id, range_end_msg_id, summary_text, compressed_at
         FROM summaries
         WHERE session_id = ?1
         ORDER BY id DESC
         LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![session_id], |row| {
        Ok(Summary {
            id: Some(row.get(0)?),
            session_id: row.get(1)?,
            range_start_msg_id: row.get(2)?,
            range_end_msg_id: row.get(3)?,
            summary_text: row.get(4)?,
            compressed_at: Some(row.get(5)?),
        })
    })?;
    rows.next().transpose()
}

/// Count total messages across all sessions.
pub fn count_messages(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
}

/// Total token usage across all sessions, for the last N days.
pub fn total_tokens(conn: &Connection, days: usize) -> Result<(i64, i64)> {
    let offset = format!("-{} days", days);
    let mut stmt = conn.prepare(
        "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0)
         FROM token_usage
         WHERE date >= date('now', ?1)",
    )?;
    stmt.query_row(params![offset], |row| Ok((row.get(0)?, row.get(1)?)))
}

/// Record or accumulate token usage for a session + model on today's date.
/// Uses `ON CONFLICT` so calling it multiple times per day sums the counts.
pub fn upsert_token_usage(
    conn: &Connection,
    session_id: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO token_usage (date, session_id, model, input_tokens, output_tokens)
         VALUES (date('now'), ?1, ?2, ?3, ?4)
         ON CONFLICT(date, session_id, model) DO UPDATE SET
             input_tokens  = input_tokens + ?3,
             output_tokens = output_tokens + ?4",
        params![session_id, model, input_tokens, output_tokens],
    )?;
    Ok(())
}

/// Update the summary text stored directly on the sessions row.
pub fn update_session_summary(
    conn: &Connection,
    session_id: &str,
    summary: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET summary = ?2 WHERE id = ?1",
        params![session_id, summary],
    )?;
    Ok(())
}

/// Insert a user prompt into the input history table.
pub fn insert_input_history(
    conn: &Connection,
    session_id: &str,
    prompt: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO input_history (session_id, prompt, created_at)
         VALUES (?1, ?2, datetime('now'))",
        params![session_id, prompt],
    )?;
    Ok(())
}

/// List the most recent N input history entries, newest first.
pub fn list_input_history(conn: &Connection, limit: usize) -> Result<Vec<InputHistoryEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, prompt, created_at
         FROM input_history
         ORDER BY id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(InputHistoryEntry {
            id: row.get(0)?,
            session_id: row.get(1)?,
            prompt: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    rows.collect()
}

// ── Memory (memories table) ─────────────────────────────────────

/// List all memory facts for a user, newest first.
pub fn list_memories(conn: &Connection, user_id: &str) -> Result<Vec<MemoryFact>> {
    let mut stmt = conn.prepare(
        "SELECT id, user_id, key, value, source, created_at, updated_at
         FROM memories
         WHERE user_id = ?1
         ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map(params![user_id], |row| {
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
    rows.collect()
}

/// Insert or update a memory fact (`ON CONFLICT` upserts on user_id + key).
pub fn upsert_memory(
    conn: &Connection,
    user_id: &str,
    key: &str,
    value: &str,
    source: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO memories (user_id, key, value, source, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'), datetime('now'))
         ON CONFLICT(user_id, key) DO UPDATE SET
             value = ?3, source = ?4, updated_at = datetime('now')",
        params![user_id, key, value, source],
    )?;
    Ok(())
}

/// Delete a single memory fact. Returns `true` if a row was deleted.
pub fn delete_memory(conn: &Connection, user_id: &str, key: &str) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM memories WHERE user_id = ?1 AND key = ?2",
        params![user_id, key],
    )?;
    Ok(n > 0)
}

/// Delete all memory facts for a user. Returns the number of rows deleted.
pub fn clear_memories(conn: &Connection, user_id: &str) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM memories WHERE user_id = ?1",
        params![user_id],
    )?;
    Ok(n)
}
