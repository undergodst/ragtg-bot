//! Chat events: автоматически наполняемая векторная память значимых моментов чата.
//! Заменяет ручной лор. Запись (Phase 3) и чтение (Phase 1).

use std::collections::HashMap;

use qdrant_client::qdrant::Value as QdrantValue;
use uuid::Uuid;

use crate::deps::Deps;
use crate::storage::qdrant as qdrant_store;

/// Категории событий, как их размечает LLM-скорер (Phase 3).
/// Сейчас используется только в payload и метриках.
pub const CATEGORIES: &[&str] = &[
    "quote", "event", "meme", "conflict", "fact", "media", "banger",
];

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
