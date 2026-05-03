//! Chat events: автоматически наполняемая векторная память значимых моментов чата.
//! Заменяет ручной лор. Запись (Phase 3) и чтение (Phase 1).

use std::collections::HashMap;

use qdrant_client::qdrant::Value as QdrantValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::deps::Deps;
use crate::storage::qdrant as qdrant_store;
use crate::storage::redis as redis_store;

/// Категории событий, как их размечает LLM-скорер (Phase 3).
/// Сейчас используется только в payload и метриках.
pub const CATEGORIES: &[&str] = &[
    "quote", "event", "meme", "conflict", "fact", "media", "banger",
];

/// Минимальная длина текста (без пробелов) чтобы пройти heuristic-фильтр.
const HEURISTIC_MIN_TEXT_LEN: usize = 15;

/// JSON-payload, который мы сериализуем в Redis-буфер кандидатов.
/// Скорер потом десериализует и формирует промпт. Поля совпадают с тем
/// что нужно `events::insert` после успешного скоринга.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateRow {
    pub sqlite_message_id: i64,
    pub user_id: i64,
    pub username: Option<String>,
    pub text: String,
    pub media_desc: Option<String>,
}

/// Эвристика Stage 1: пропускаем явный мусор без LLM.
/// - Слэш-команды: `/start`, `/help` и т.п.
/// - Слишком короткий текст без медиа.
/// - Полностью пустое сообщение (стикер без описания).
pub fn is_candidate(text: &str, media_desc: Option<&str>) -> bool {
    let t = text.trim();
    if t.starts_with('/') {
        return false;
    }
    let text_len_no_ws = t.chars().filter(|c| !c.is_whitespace()).count();
    let has_text = text_len_no_ws >= HEURISTIC_MIN_TEXT_LEN;
    let has_media = media_desc.map(|s| !s.trim().is_empty()).unwrap_or(false);
    has_text || has_media
}

/// SHA256 от нормализованного `text + "\n" + media_desc` — ключ для дедуп-сета.
fn dedup_hash(text: &str, media_desc: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.trim().to_lowercase().as_bytes());
    hasher.update(b"\n");
    if let Some(d) = media_desc {
        hasher.update(d.trim().to_lowercase().as_bytes());
    }
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Пропустить через эвристику + дедуп-сет, при выживании — RPUSH в буфер.
/// Возвращает текущую длину буфера (или 0 если кандидат отфильтрован).
/// Best-effort: на любой Redis-ошибке тихо логирует и возвращает 0.
pub async fn enqueue_candidate(deps: &Deps, chat_id: i64, row: &CandidateRow) -> i64 {
    if !is_candidate(&row.text, row.media_desc.as_deref()) {
        return 0;
    }
    let hash = dedup_hash(&row.text, row.media_desc.as_deref());
    match redis_store::record_unique_event_hash(&deps.redis, chat_id, &hash).await {
        Ok(true) => {} // first time we see this — proceed
        Ok(false) => {
            tracing::debug!(chat_id, "event candidate skipped: exact dupe within 1h");
            return 0;
        }
        Err(e) => {
            tracing::warn!(error = %e, chat_id, "dedup-set check failed; enqueuing anyway");
        }
    }
    let payload = match serde_json::to_string(row) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "candidate serialize failed");
            return 0;
        }
    };
    match redis_store::push_event_candidate(&deps.redis, chat_id, &payload).await {
        Ok(len) => {
            crate::metrics::EVENTS_CANDIDATE_TOTAL.inc();
            tracing::debug!(chat_id, buffer_len = len, "event candidate enqueued");
            len
        }
        Err(e) => {
            tracing::warn!(error = %e, chat_id, "push event candidate failed");
            0
        }
    }
}

/// Сохранить событие: текст в SQLite, вектор в Qdrant.
/// Используется в Phase 3 фоновым скорером.
#[allow(dead_code)]
pub async fn insert(
    deps: &Deps,
    chat_id: i64,
    source_message_id: Option<i64>,
    text: &str,
    category: &str,
    score: u8,
    vector: Vec<f32>,
) -> anyhow::Result<i64> {
    let point_id = Uuid::new_v4().to_string();
    let score_i64 = score as i64;

    sqlx::query!(
        r#"INSERT INTO chat_events (chat_id, source_message_id, text, category, score, qdrant_point_id)
           VALUES (?, ?, ?, ?, ?, ?)"#,
        chat_id,
        source_message_id,
        text,
        category,
        score_i64,
        point_id,
    )
    .execute(&deps.sqlite)
    .await?;

    let sqlite_id = sqlx::query_scalar!(
        r#"SELECT id AS "id!: i64" FROM chat_events WHERE qdrant_point_id = ? LIMIT 1"#,
        point_id
    )
    .fetch_one(&deps.sqlite)
    .await?;

    let mut payload: HashMap<String, QdrantValue> = HashMap::new();
    payload.insert("chat_id".into(), QdrantValue::from(chat_id));
    payload.insert("sqlite_id".into(), QdrantValue::from(sqlite_id));
    payload.insert("category".into(), QdrantValue::from(category.to_string()));
    payload.insert("score".into(), QdrantValue::from(score_i64));

    qdrant_store::upsert_point(&deps.qdrant, "chat_events", &point_id, vector, payload).await?;

    crate::metrics::EVENTS_STORED_TOTAL.inc();
    tracing::info!(chat_id, sqlite_id, category, score, "chat event stored");
    Ok(sqlite_id)
}

/// Достать top-K событий по семантической близости к запросу.
/// Best-effort: при ошибках возвращает пустой вектор, не блокирует ответ.
pub async fn retrieve_relevant(
    deps: &Deps,
    chat_id: i64,
    vector: &[f32],
) -> Vec<String> {
    match retrieve_inner(deps, chat_id, vector).await {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!(error = %e, chat_id, "chat_events retrieval failed; proceeding without");
            Vec::new()
        }
    }
}

async fn retrieve_inner(
    deps: &Deps,
    chat_id: i64,
    vector: &[f32],
) -> anyhow::Result<Vec<String>> {
    let top_k = deps.config.memory.top_k_events;
    if top_k == 0 {
        return Ok(Vec::new());
    }

    let hits = qdrant_store::search_similar(
        &deps.qdrant,
        "chat_events",
        vector.to_vec(),
        chat_id,
        top_k,
    )
    .await?;

    let mut out = Vec::new();
    for hit in &hits {
        let sqlite_id = hit
            .payload
            .get("sqlite_id")
            .and_then(|v| v.kind.as_ref())
            .and_then(|k| match k {
                qdrant_client::qdrant::value::Kind::IntegerValue(i) => Some(*i),
                _ => None,
            });
        if let Some(id) = sqlite_id {
            let text = sqlx::query_scalar!(
                r#"SELECT text FROM chat_events WHERE id = ?"#,
                id
            )
            .fetch_optional(&deps.sqlite)
            .await
            .ok()
            .flatten();
            if let Some(t) = text {
                out.push(t);
            }
        }
    }

    if !out.is_empty() {
        tracing::info!(chat_id, count = out.len(), "chat_events retrieved");
    }
    Ok(out)
}
