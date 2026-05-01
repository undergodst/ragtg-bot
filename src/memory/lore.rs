//! Lore memory: chat knowledge base (memes, inside jokes, historical events).
//!
//! - Manual: `/lore_add`, `/lore_list`, `/lore_del` admin commands
//! - Retrieval: top-K relevant lore entries via Qdrant vector search

use crate::deps::Deps;
use crate::storage::qdrant as qdrant_store;

/// Retrieve the top-K most relevant lore entries for `chat_id`.
/// Best-effort: errors produce an empty vec.
pub async fn retrieve_relevant_lore(
    deps: &Deps,
    chat_id: i64,
    query_text: &str,
) -> Vec<String> {
    match retrieve_inner(deps, chat_id, query_text).await {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(
                error = %e,
                chat_id,
                "lore retrieval failed; proceeding without lore context"
            );
            Vec::new()
        }
    }
}

async fn retrieve_inner(
    deps: &Deps,
    chat_id: i64,
    query_text: &str,
) -> anyhow::Result<Vec<String>> {
    let top_k = deps.config.memory.top_k_lore;
    if top_k == 0 {
        return Ok(Vec::new());
    }

    let vector = deps.embeddings.embed_single(query_text).await?;

    let hits = qdrant_store::search_similar(
        &deps.qdrant,
        "lore",
        vector,
        chat_id,
        top_k,
    )
    .await?;

    // Fetch lore text from SQLite.
    let mut entries = Vec::new();
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
                r#"SELECT text FROM lore WHERE id = ?"#,
                id
            )
            .fetch_optional(&deps.sqlite)
            .await
            .ok()
            .flatten();
            if let Some(t) = text {
                entries.push(t);
            }
        }
    }

    if !entries.is_empty() {
        tracing::info!(chat_id, count = entries.len(), "lore retrieved");
    }

    Ok(entries)
}

/// Add a lore entry: store text in SQLite, embed and upsert into Qdrant.
pub async fn add_lore(
    deps: &Deps,
    chat_id: i64,
    text: &str,
    added_by: Option<i64>,
    tags: Option<&str>,
) -> anyhow::Result<i64> {
    let vector = deps.embeddings.embed_single(text).await?;
    let point_id = uuid::Uuid::new_v4().to_string();

    sqlx::query!(
        r#"INSERT INTO lore (chat_id, text, tags, qdrant_point_id, added_by)
           VALUES (?, ?, ?, ?, ?)"#,
        chat_id,
        text,
        tags,
        point_id,
        added_by
    )
    .execute(&deps.sqlite)
    .await?;

    let sqlite_id = sqlx::query_scalar!(
        r#"SELECT id AS "id!: i64" FROM lore WHERE qdrant_point_id = ? LIMIT 1"#,
        point_id
    )
    .fetch_one(&deps.sqlite)
    .await?;

    let mut payload = std::collections::HashMap::new();
    payload.insert("chat_id".into(), qdrant_client::qdrant::Value::from(chat_id));
    payload.insert("sqlite_id".into(), qdrant_client::qdrant::Value::from(sqlite_id));
    if let Some(t) = tags {
        payload.insert("tags".into(), qdrant_client::qdrant::Value::from(t.to_string()));
    }

    qdrant_store::upsert_point(&deps.qdrant, "lore", &point_id, vector, payload).await?;

    tracing::info!(chat_id, sqlite_id, "lore entry added");
    Ok(sqlite_id)
}

/// Delete a lore entry by SQLite ID. Also removes from Qdrant.
pub async fn delete_lore(deps: &Deps, lore_id: i64) -> anyhow::Result<bool> {
    // Get the qdrant point ID before deleting.
    let point_id = sqlx::query_scalar!(
        r#"SELECT qdrant_point_id FROM lore WHERE id = ?"#,
        lore_id
    )
    .fetch_optional(&deps.sqlite)
    .await?;

    let Some(point_id) = point_id else {
        return Ok(false);
    };

    sqlx::query!(r#"DELETE FROM lore WHERE id = ?"#, lore_id)
        .execute(&deps.sqlite)
        .await?;

    // Best-effort Qdrant delete.
    if let Err(e) = qdrant_store::delete_point(&deps.qdrant, "lore", &point_id).await {
        tracing::warn!(error = %e, "qdrant lore delete failed");
    }

    tracing::info!(lore_id, "lore entry deleted");
    Ok(true)
}

/// List all lore entries for a chat (for /lore_list command).
pub async fn list_lore(deps: &Deps, chat_id: i64) -> anyhow::Result<Vec<(i64, String)>> {
    let rows = sqlx::query!(
        r#"SELECT id AS "id!: i64", text FROM lore WHERE chat_id = ? ORDER BY created_at DESC"#,
        chat_id
    )
    .fetch_all(&deps.sqlite)
    .await?;

    Ok(rows.into_iter().map(|r| (r.id, r.text)).collect())
}
