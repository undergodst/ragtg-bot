//! Episodic memory retrieval: embed the query, search Qdrant for relevant
//! summaries, return them as strings for injection into the prompt.

use crate::deps::Deps;
use crate::storage::qdrant as qdrant_store;

/// Retrieve the top-K most relevant episodic summaries for `chat_id`,
/// ranked by vector similarity to `query_text`. Returns an empty vec
/// on any error (retrieval is best-effort — never blocks a reply).
pub async fn retrieve_relevant_summaries(
    deps: &Deps,
    chat_id: i64,
    vector: &[f32],
) -> Vec<String> {
    match retrieve_inner(deps, chat_id, vector).await {
        Ok(summaries) => summaries,
        Err(e) => {
            tracing::warn!(
                error = %e,
                chat_id,
                "episodic retrieval failed; proceeding without long-term context"
            );
            Vec::new()
        }
    }
}

async fn retrieve_inner(
    deps: &Deps,
    chat_id: i64,
    vector: &[f32],
) -> anyhow::Result<Vec<String>> {
    let top_k = deps.config.memory.top_k_summaries;
    if top_k == 0 {
        return Ok(Vec::new());
    }

    let hits = qdrant_store::search_similar(
        &deps.qdrant,
        "episodic_summaries",
        vector.to_vec(),
        chat_id,
        top_k,
    )
    .await?;

    let summaries: Vec<String> = hits
        .into_iter()
        .filter_map(|hit| {
            hit.payload
                .get("text")
                .and_then(|v| v.kind.as_ref())
                .and_then(|k| match k {
                    qdrant_client::qdrant::value::Kind::StringValue(s) => Some(s.clone()),
                    _ => None,
                })
        })
        .collect();

    if !summaries.is_empty() {
        tracing::info!(
            chat_id,
            count = summaries.len(),
            "episodic context retrieved"
        );
    }

    Ok(summaries)
}
