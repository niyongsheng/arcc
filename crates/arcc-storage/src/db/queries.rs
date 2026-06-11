//! Query helpers for inspecting persisted data in the TUI `/data` command.
//!
//! All functions take a `&rusqlite::Connection` and are meant to be called
//! from `ArccStorage` methods which manage the mutex lock.

use rusqlite::{params, Connection, Result};

use super::models::{Message, Session, Summary};

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
        "SELECT id, name, mode, created_at, last_active_at
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
