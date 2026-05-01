use std::sync::Arc;

use deadpool_redis::Pool as RedisPool;
use qdrant_client::Qdrant;
use sqlx::SqlitePool;

use crate::config::Config;
use crate::llm::client::OpenRouterClient;
use crate::llm::embeddings::EmbeddingClient;

/// Shared dependencies injected into bot handlers and HTTP servers.
#[derive(Clone)]
pub struct Deps {
    pub sqlite: SqlitePool,
    pub qdrant: Arc<Qdrant>,
    pub redis: RedisPool,
    pub openrouter: OpenRouterClient,
    pub embeddings: EmbeddingClient,
    pub config: Arc<Config>,
}
