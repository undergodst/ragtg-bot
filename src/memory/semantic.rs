//! Semantic memory retrieval: retrieve relevant facts about users
//! from Qdrant for injection into the LLM prompt.

use std::collections::HashMap;

use crate::deps::Deps;
use crate::storage::qdrant as qdrant_store;

/// Retrieve the top-K most relevant facts about `user_id` in `chat_id`.
/// Returns fact strings. Best-effort: errors produce an empty vec.
pub async fn retrieve_user_facts(
    deps: &Deps,
    chat_id: i64,
    user_id: i64,
    query_text: &str,
) -> Vec<String> {
    match retrieve_inner(deps, chat_id, user_id, query_text).await {
        Ok(facts) => facts,
        Err(e) => {
            tracing::warn!(
                error = %e,
                chat_id,
                user_id,
                "semantic fact retrieval failed; proceeding without user facts"
            );
            Vec::new()
        }
    }
}

async fn retrieve_inner(
    deps: &Deps,
    chat_id: i64,
    user_id: i64,
    query_text: &str,
) -> anyhow::Result<Vec<String>> {
    let top_k = deps.config.memory.top_k_facts;
    if top_k == 0 {
        return Ok(Vec::new());
    }

    let vector = deps.embeddings.embed_single(query_text).await?;

    let hits = qdrant_store::search_similar_user_facts(
        &deps.qdrant,
        vector,
        chat_id,
        user_id,
        top_k,
    )
    .await?;

    // Fetch fact text from SQLite using sqlite_ids from payload.
    let mut facts = Vec::new();
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
            let fact_text = sqlx::query_scalar!(
                r#"SELECT fact FROM user_facts WHERE id = ?"#,
                id
            )
            .fetch_optional(&deps.sqlite)
            .await
            .ok()
            .flatten();
            if let Some(f) = fact_text {
                facts.push(f);
            }
        }
    }

    if !facts.is_empty() {
        tracing::info!(
            chat_id,
            user_id,
            count = facts.len(),
            "user facts retrieved"
        );
    }

    Ok(facts)
}

/// Collect facts for all unique users in the working memory window.
/// Returns a map: username/display_name → Vec<fact_string>.
pub async fn retrieve_facts_for_window_users(
    deps: &Deps,
    chat_id: i64,
    window: &[crate::memory::working::WorkingMessage],
    query_text: &str,
) -> HashMap<String, Vec<String>> {
    let mut result: HashMap<String, Vec<String>> = HashMap::new();

    // Collect unique user_ids from the window.
    let mut seen_users = std::collections::HashSet::new();
    for msg in window {
        seen_users.insert(msg.user_id);
    }

    for user_id in seen_users {
        let facts = retrieve_user_facts(deps, chat_id, user_id, query_text).await;
        if !facts.is_empty() {
            let display = window
                .iter()
                .find(|m| m.user_id == user_id)
                .and_then(|m| m.username.clone())
                .unwrap_or_else(|| format!("uid:{user_id}"));
            result.insert(display, facts);
        }
    }

    result
}
