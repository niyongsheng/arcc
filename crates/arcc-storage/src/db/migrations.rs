use rusqlite::Connection;
use tracing::info;

/// Run all schema migrations (idempotent — uses `IF NOT EXISTS`).
pub fn run(conn: &Connection) -> Result<(), rusqlite::Error> {
    let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;

    if version < 1 {
        info!("running migration v1: create core tables");
        conn.execute_batch(MIGRATION_V1)?;
        conn.pragma_update(None, "user_version", 1)?;
    }

    if version < 2 {
        info!("running migration v2: token_usage unique constraint");
        conn.execute_batch(MIGRATION_V2)?;
        conn.pragma_update(None, "user_version", 2)?;
    }

    if version < 3 {
        info!("running migration v3: session summary + input_history");
        conn.execute_batch(MIGRATION_V3)?;
        conn.pragma_update(None, "user_version", 3)?;
    }

    if version < 4 {
        info!("running migration v4: memories table");
        conn.execute_batch(MIGRATION_V4)?;
        conn.pragma_update(None, "user_version", 4)?;
    }

    if version < 5 {
        info!("running migration v5: scheduled_tasks table");
        conn.execute_batch(MIGRATION_V5)?;
        conn.pragma_update(None, "user_version", 5)?;
    }

    if version < 6 {
        info!("running migration v6: add paused status to scheduled_tasks");
        conn.execute_batch(MIGRATION_V6)?;
        conn.pragma_update(None, "user_version", 6)?;
    }

    if version < 7 {
        info!("running migration v7: add 'acp' mode to sessions");
        conn.execute_batch(MIGRATION_V7)?;
        conn.pragma_update(None, "user_version", 7)?;
    }

    if version < 8 {
        info!("running migration v8: add dingtalk platform + session mode");
        // The original v7 rebuild dropped the `summary` column. Preserve it
        // when present; databases already at v7 no longer have it.
        let has_summary = {
            let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
            let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
            columns
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|c| c == "summary")
        };
        conn.execute_batch(if has_summary {
            MIGRATION_V8
        } else {
            MIGRATION_V8_WITHOUT_SUMMARY
        })?;
        conn.pragma_update(None, "user_version", 8)?;
    }

    Ok(())
}

const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    mode            TEXT NOT NULL CHECK(mode IN ('tui','cli','server','feishu')),
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    last_active_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role        TEXT NOT NULL CHECK(role IN ('system','user','assistant','tool')),
    content     TEXT NOT NULL,
    token_count INTEGER,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS summaries (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    range_start_msg_id  INTEGER NOT NULL REFERENCES messages(id),
    range_end_msg_id    INTEGER NOT NULL REFERENCES messages(id),
    summary_text        TEXT NOT NULL,
    compressed_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS token_usage (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    date            TEXT NOT NULL DEFAULT (date('now')),
    session_id      TEXT NOT NULL REFERENCES sessions(id),
    model           TEXT NOT NULL,
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_token_usage_date  ON token_usage(date);
CREATE INDEX IF NOT EXISTS idx_summaries_session  ON summaries(session_id);
"#;

const MIGRATION_V2: &str = r#"
CREATE UNIQUE INDEX IF NOT EXISTS idx_token_usage_unique
    ON token_usage(date, session_id, model);
"#;

const MIGRATION_V3: &str = r#"
ALTER TABLE sessions ADD COLUMN summary TEXT;

CREATE TABLE IF NOT EXISTS input_history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    prompt     TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_input_history_created ON input_history(created_at DESC);
"#;

const MIGRATION_V4: &str = r#"
CREATE TABLE IF NOT EXISTS memories (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT NOT NULL,
    key         TEXT NOT NULL,
    value       TEXT NOT NULL,
    source      TEXT NOT NULL DEFAULT 'extraction'
                CHECK(source IN ('extraction', 'manual')),
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(user_id, key)
);

CREATE INDEX IF NOT EXISTS idx_memories_user ON memories(user_id);
"#;

const MIGRATION_V5: &str = r#"
CREATE TABLE IF NOT EXISTS scheduled_tasks (
    id              TEXT PRIMARY KEY,
    chat_id         TEXT NOT NULL,
    chat_type       TEXT NOT NULL,
    open_id         TEXT NOT NULL,
    reply_id        TEXT NOT NULL,
    reply_id_type   TEXT NOT NULL,
    cron            TEXT,
    task_description TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','running','completed','failed')),
    next_run_at     TEXT NOT NULL,
    last_run_at     TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_due
    ON scheduled_tasks(status, next_run_at);
"#;

const MIGRATION_V6: &str = r#"
-- Recreate scheduled_tasks with 'paused' status added to CHECK constraint.
-- SQLite cannot ALTER CHECK, so we recreate the table.
CREATE TABLE IF NOT EXISTS scheduled_tasks_v6 (
    id              TEXT PRIMARY KEY,
    chat_id         TEXT NOT NULL,
    chat_type       TEXT NOT NULL,
    open_id         TEXT NOT NULL,
    reply_id        TEXT NOT NULL,
    reply_id_type   TEXT NOT NULL,
    cron            TEXT,
    task_description TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','running','paused','completed','failed')),
    next_run_at     TEXT NOT NULL,
    last_run_at     TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO scheduled_tasks_v6
    SELECT id, chat_id, chat_type, open_id, reply_id, reply_id_type,
           cron, task_description, status, next_run_at, last_run_at,
           created_at, updated_at
    FROM scheduled_tasks;

DROP TABLE scheduled_tasks;
ALTER TABLE scheduled_tasks_v6 RENAME TO scheduled_tasks;

CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_due
    ON scheduled_tasks(status, next_run_at);
"#;

const MIGRATION_V8: &str = r#"
-- v8: distinguish scheduled-task origin platforms and allow dingtalk sessions.
-- Existing tasks predate DingTalk support, so they default to 'feishu'.
ALTER TABLE scheduled_tasks ADD COLUMN platform TEXT NOT NULL DEFAULT 'feishu';

-- SQLite cannot ALTER CHECK, so sessions is recreated again with 'dingtalk'.
PRAGMA foreign_keys = OFF;

CREATE TABLE IF NOT EXISTS sessions_v8 (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    mode            TEXT NOT NULL CHECK(mode IN ('tui','cli','server','feishu','acp','dingtalk')),
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    last_active_at  TEXT NOT NULL DEFAULT (datetime('now')),
    summary         TEXT
);

INSERT INTO sessions_v8
    SELECT id, name, mode, created_at, last_active_at, summary
    FROM sessions;

DROP TABLE sessions;
ALTER TABLE sessions_v8 RENAME TO sessions;

PRAGMA foreign_keys = ON;
"#;

/// v8 variant for databases whose sessions table lost `summary` in the
/// original (buggy) v7 rebuild.
const MIGRATION_V8_WITHOUT_SUMMARY: &str = r#"
PRAGMA foreign_keys = OFF;

CREATE TABLE IF NOT EXISTS sessions_v8 (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    mode            TEXT NOT NULL CHECK(mode IN ('tui','cli','server','feishu','acp','dingtalk')),
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    last_active_at  TEXT NOT NULL DEFAULT (datetime('now')),
    summary         TEXT
);

INSERT INTO sessions_v8 (id, name, mode, created_at, last_active_at)
    SELECT id, name, mode, created_at, last_active_at
    FROM sessions;

DROP TABLE sessions;
ALTER TABLE sessions_v8 RENAME TO sessions;

PRAGMA foreign_keys = ON;
"#;

const MIGRATION_V7: &str = r#"
-- Recreate sessions with 'acp' added to the mode CHECK constraint.
-- SQLite cannot ALTER CHECK, so we recreate the table. Messages and
-- other tables hold FK references to sessions(id), so foreign-key
-- enforcement must be off while the parent table is being replaced.
PRAGMA foreign_keys = OFF;

CREATE TABLE IF NOT EXISTS sessions_v7 (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    mode            TEXT NOT NULL CHECK(mode IN ('tui','cli','server','feishu','acp')),
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    last_active_at  TEXT NOT NULL DEFAULT (datetime('now')),
    summary         TEXT
);

INSERT INTO sessions_v7
    SELECT id, name, mode, created_at, last_active_at, summary
    FROM sessions;

DROP TABLE sessions;
ALTER TABLE sessions_v7 RENAME TO sessions;

PRAGMA foreign_keys = ON;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_to_latest_with_dingtalk_support() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        run(&conn).expect("migrations run");

        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("user_version");
        assert_eq!(version, 8);

        // Sessions accept the new platform mode.
        conn.execute(
            "INSERT INTO sessions (id, name, mode) VALUES ('s1', 'dingtalk:cid', 'dingtalk')",
            [],
        )
        .expect("insert dingtalk session");

        // Existing tasks default to feishu; new tasks can select a platform.
        conn.execute(
            "INSERT INTO scheduled_tasks
                (id, chat_id, chat_type, open_id, reply_id, reply_id_type,
                 task_description, next_run_at)
             VALUES ('t1', 'c', 'p2p', 'u', 'u', 'open_id', 'task', datetime('now'))",
            [],
        )
        .expect("insert default-platform task");
        conn.execute(
            "INSERT INTO scheduled_tasks
                (id, platform, chat_id, chat_type, open_id, reply_id, reply_id_type,
                 task_description, next_run_at)
             VALUES ('t2', 'dingtalk', 'dingtalk:c', 'group', 'u', 'conversation',
                     'dingtalk_conversation_id', 'task', datetime('now'))",
            [],
        )
        .expect("insert dingtalk task");

        let platform: String = conn
            .query_row(
                "SELECT platform FROM scheduled_tasks WHERE id = 't1'",
                [],
                |row| row.get(0),
            )
            .expect("default platform");
        assert_eq!(platform, "feishu");
    }

    #[test]
    fn migrates_from_legacy_v7_without_summary() {
        // Simulate a database already at v7 whose original rebuild dropped
        // the `summary` column.
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(MIGRATION_V1).expect("v1 schema");
        conn.execute_batch(MIGRATION_V5).expect("v5 schema");
        conn.pragma_update(None, "user_version", 7)
            .expect("set version");

        run(&conn).expect("v8 migration");

        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("user_version");
        assert_eq!(version, 8);

        conn.execute(
            "INSERT INTO sessions (id, name, mode) VALUES ('s1', 'dingtalk:cid', 'dingtalk')",
            [],
        )
        .expect("dingtalk mode accepted");
    }
}
