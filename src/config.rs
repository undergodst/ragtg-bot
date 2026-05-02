use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::Deserialize;

use crate::error::Result;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub bot: BotConfig,
    pub openrouter: OpenRouterConfig,
    pub embeddings: EmbeddingsConfig,
    pub sqlite: SqliteConfig,
    pub qdrant: QdrantConfig,
    pub redis: RedisConfig,
    pub memory: MemoryConfig,
    pub ratelimit: RateLimitConfig,
    pub decision: DecisionConfig,
    pub observability: ObservabilityConfig,
    pub secrets: Secrets,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BotConfig {
    pub admin_ids: Vec<i64>,
    pub default_personality: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenRouterConfig {
    pub base_url: String,
    pub model_main: String,
    pub model_pro: String,
    pub model_ask_free: String,
    pub model_vision: String,
    pub model_decision: String,
    pub vision_fallbacks: Vec<String>,
    pub timeout_sec: u64,
    pub max_retries: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingsConfig {
    pub base_url: String,
    pub embedding_model: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SqliteConfig {
    pub path: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QdrantConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfig {
    pub working_window_size: u32,
    pub working_ttl_days: u32,
    pub episodic_summary_every_n: u32,
    pub episodic_summary_lookback: u32,
    pub facts_extraction_every_n: u32,
    pub facts_lookback: u32,
    pub top_k_summaries: u32,
    pub top_k_facts: u32,
    pub top_k_lore: u32,
    pub fact_dedup_threshold: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    pub user_cooldown_sec: u32,
    pub chat_max_per_min: u32,
    pub vision_concurrent: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecisionConfig {
    pub mention_p: f32,
    pub reply_p: f32,
    pub name_in_text_p: f32,
    pub question_after_silence_p: f32,
    pub silence_threshold_min: u32,
    pub random_p: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ObservabilityConfig {
    pub metrics_port: u16,
    pub healthz_port: u16,
    pub log_level: String,
}

#[derive(Clone, Deserialize)]
pub struct Secrets {
    pub tg_bot_token: String,
    pub or_api_key: String,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("tg_bot_token", &"[REDACTED]")
            .field("or_api_key", &"[REDACTED]")
            .finish()
    }
}

impl Config {
    /// Load config from TOML file (`CONFIG_PATH` env or `config/config.toml`)
    /// and override secrets from `TG_BOT_TOKEN`, `OR_API_KEY`, `DEEPINFRA_KEY`.
    pub fn load() -> Result<Self> {
        let path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config/config.toml".into());

        let cfg: Config = Figment::new()
            .merge(Toml::file(&path))
            .merge(
                Env::raw()
                    .only(&["TG_BOT_TOKEN", "OR_API_KEY"])
                    .map(|key| match key.as_str().to_ascii_uppercase().as_str() {
                        "TG_BOT_TOKEN" => "secrets.tg_bot_token".into(),
                        "OR_API_KEY" => "secrets.or_api_key".into(),
                        _ => key.into(),
                    }),
            )
            .extract()
            .map_err(Box::new)?;
        Ok(cfg)
    }
}
