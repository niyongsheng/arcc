use rusqlite::Connection;
use std::path::Path;
use tracing::info;

/// Open (or create) the ARCC SQLite database at `path`.
///
/// Enables WAL mode, NORMAL synchronous, and foreign keys on every open.
pub fn open(path: &Path) -> Result<Connection, rusqlite::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let conn = Connection::open(path)?;

    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous   = NORMAL;
        PRAGMA foreign_keys  = ON;
        ",
    )?;

    info!(path = %path.display(), "sqlite database opened (WAL)");
    Ok(conn)
}
