use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] sqlx::Error),

    #[error("sqlite migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("qdrant error: {0}")]
    Qdrant(String),

    #[error("redis error: {0}")]
    Redis(String),

    #[error("openrouter error: {0}")]
    OpenRouter(String),

    #[error("telegram error: {0}")]
    Telegram(String),

    #[error("config error: {0}")]
    Config(#[from] Box<figment::Error>),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("env error: {0}")]
    Env(#[from] std::env::VarError),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
