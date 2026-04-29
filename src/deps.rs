use std::sync::Arc;

use deadpool_redis::Pool as RedisPool;
use qdrant_client::Qdrant;
use sqlx::SqlitePool;

use crate::config::Config;

/// Shared dependencies injected into bot handlers and HTTP servers.
#[derive(Clone)]
pub struct Deps {
    pub sqlite: SqlitePool,
    pub qdrant: Arc<Qdrant>,
    pub redis: RedisPool,
    pub config: Arc<Config>,
}
