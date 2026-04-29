use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::error::Result;

/// Build a SQLite pool with WAL, foreign keys, NORMAL synchronous and 5s busy timeout.
pub async fn init_pool(path: &str, max_connections: u32) -> Result<SqlitePool> {
    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_millis(5000));

    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(opts)
        .await?;

    Ok(pool)
}

/// Run all migrations from `./migrations` (baked into the binary at compile time).
pub async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

/// `SELECT 1` smoke check.
pub async fn healthcheck(pool: &SqlitePool) -> Result<()> {
    let _: i64 = sqlx::query_scalar("SELECT 1").fetch_one(pool).await?;
    Ok(())
}
