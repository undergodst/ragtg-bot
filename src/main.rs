mod bot;
mod config;
mod decision;
mod deps;
mod error;
mod llm;
mod memory;
mod personality;
mod storage;
mod tasks;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use serde::Serialize;
use teloxide::Bot;
use tokio::try_join;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::deps::Deps;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = Config::load()?;
    tracing::info!(
        sqlite = %config.sqlite.path,
        qdrant = %config.qdrant.url,
        redis = %config.redis.url,
        healthz_port = config.observability.healthz_port,
        metrics_port = config.observability.metrics_port,
        "config loaded"
    );

    let (sqlite_pool, qdrant_client, redis_pool) = try_join!(
        async { storage::sqlite::init_pool(&config.sqlite.path, config.sqlite.max_connections).await },
        async { storage::qdrant::init_client(&config.qdrant.url) },
        async { storage::redis::init_pool(&config.redis.url) },
    )?;
    tracing::info!("storages initialised");

    storage::sqlite::run_migrations(&sqlite_pool).await?;
    tracing::info!("sqlite migrations applied");

    storage::qdrant::ensure_collections(&qdrant_client).await?;
    tracing::info!("qdrant collections ensured");

    storage::redis::healthcheck(&redis_pool).await?;
    tracing::info!("redis ping ok");

    let openrouter = llm::client::OpenRouterClient::new(
        config.openrouter.base_url.clone(),
        config.secrets.or_api_key.clone(),
        config.openrouter.timeout_sec,
        config.openrouter.max_retries,
    )?;

    let deps = Deps {
        sqlite: sqlite_pool,
        qdrant: Arc::new(qdrant_client),
        redis: redis_pool,
        openrouter,
        config: Arc::new(config),
    };

    run(deps).await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,teloxide=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

async fn run(deps: Deps) -> anyhow::Result<()> {
    let healthz_port = deps.config.observability.healthz_port;
    let metrics_port = deps.config.observability.metrics_port;

    let healthz_app = Router::new()
        .route("/healthz", get(healthz))
        .with_state(deps.clone());
    let metrics_app = Router::new().route("/metrics", get(metrics));

    let healthz_addr = SocketAddr::from(([0, 0, 0, 0], healthz_port));
    let metrics_addr = SocketAddr::from(([0, 0, 0, 0], metrics_port));

    let healthz_listener = tokio::net::TcpListener::bind(healthz_addr).await?;
    let metrics_listener = tokio::net::TcpListener::bind(metrics_addr).await?;
    tracing::info!(addr = %healthz_addr, "healthz server listening");
    tracing::info!(addr = %metrics_addr, "metrics server listening");

    let mut healthz_task = tokio::spawn(async move {
        axum::serve(healthz_listener, healthz_app)
            .await
            .map_err(|e| anyhow::anyhow!("healthz server: {e}"))
    });
    let mut metrics_task = tokio::spawn(async move {
        axum::serve(metrics_listener, metrics_app)
            .await
            .map_err(|e| anyhow::anyhow!("metrics server: {e}"))
    });

    let bot_client = Bot::new(deps.config.secrets.tg_bot_token.clone());
    let mut dispatcher = bot::build_dispatcher(bot_client, deps.clone());
    let shutdown_token = dispatcher.shutdown_token();
    let dispatcher_task = tokio::spawn(async move {
        dispatcher.dispatch().await;
        tracing::warn!("teloxide dispatcher exited");
    });
    tracing::info!("teloxide dispatcher started");

    // Healthz / metrics outliving the dispatcher is intentional: an invalid TG
    // token or a transient polling failure should not take down the observability
    // surface that operators rely on to diagnose the very problem.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown signal received");
        }
        res = &mut healthz_task => {
            tracing::error!("healthz task ended early: {res:?}");
        }
        res = &mut metrics_task => {
            tracing::error!("metrics task ended early: {res:?}");
        }
    }

    if let Ok(fut) = shutdown_token.shutdown() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), fut).await;
    }
    healthz_task.abort();
    metrics_task.abort();
    dispatcher_task.abort();
    Ok(())
}

#[derive(Serialize)]
struct HealthzResponse {
    status: &'static str,
    sqlite: ComponentStatus,
    qdrant: ComponentStatus,
    redis: ComponentStatus,
}

#[derive(Serialize)]
struct ComponentStatus {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ComponentStatus {
    fn from_result(r: error::Result<()>) -> Self {
        match r {
            Ok(()) => Self {
                ok: true,
                error: None,
            },
            Err(e) => Self {
                ok: false,
                error: Some(e.to_string()),
            },
        }
    }
}

async fn healthz(State(deps): State<Deps>) -> impl IntoResponse {
    let sqlite = ComponentStatus::from_result(storage::sqlite::healthcheck(&deps.sqlite).await);
    let qdrant = ComponentStatus::from_result(storage::qdrant::healthcheck(&deps.qdrant).await);
    let redis = ComponentStatus::from_result(storage::redis::healthcheck(&deps.redis).await);
    let all_ok = sqlite.ok && qdrant.ok && redis.ok;
    let status = if all_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = HealthzResponse {
        status: if all_ok { "ok" } else { "degraded" },
        sqlite,
        qdrant,
        redis,
    };
    (status, Json(body))
}

async fn metrics() -> impl IntoResponse {
    let body = "# HELP up Whether the bot is running\n# TYPE up gauge\nup 1\n";
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
}
