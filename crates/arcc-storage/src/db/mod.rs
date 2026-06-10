pub mod connection;
pub mod migrations;
pub mod models;
pub mod queries;

use rusqlite::Connection;

/// Open the database, run migrations, and return a ready connection.
pub fn init(path: &std::path::Path) -> Result<Connection, rusqlite::Error> {
    let conn = connection::open(path)?;
    migrations::run(&conn)?;
    Ok(conn)
}
