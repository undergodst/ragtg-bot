use std::collections::HashMap;
use qdrant_client::qdrant::{Value as QdrantValue, SearchPointsBuilder};
use serde::Deserialize;
use uuid::Uuid;

use crate::deps::Deps;
use crate::storage::qdrant as qdrant_store;

#[derive(Debug, Deserialize)]
struct FewShot {
    context: String,
    reply: String,
}

pub async fn seed_shots(deps: &Deps) -> anyhow::Result<()> {
    let shots_json = include_str!("../llm/prompts/examples/shots.json");
    let shots: Vec<FewShot> = serde_json::from_str(shots_json)?;
    
    tracing::info!(count = shots.len(), "seeding style shots into qdrant");

    for shot in shots {
        // Use deterministic UUID based on context to avoid duplicates
        let point_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, shot.context.as_bytes()).to_string();
        
        // Check if already exists? Or just upsert (idempotent)
        let vector = deps.embeddings.embed_single(&shot.context).await?;
        
        let mut payload = HashMap::new();
        payload.insert("context".into(), QdrantValue::from(shot.context));
        payload.insert("reply".into(), QdrantValue::from(shot.reply));

        qdrant_store::upsert_point(&deps.qdrant, "style_shots", &point_id, vector, payload).await?;
    }

    Ok(())
}

pub async fn retrieve_relevant_shots(deps: &Deps, vector: Vec<f32>, top_k: u32) -> Vec<(String, String)> {
    let results = deps.qdrant.search_points(
        SearchPointsBuilder::new("style_shots", vector, top_k as u64)
            .with_payload(true)
    ).await;

    match results {
        Ok(resp) => resp.result.into_iter().map(|hit| {
            let context = hit.payload.get("context").and_then(|v| v.as_str()).map(|s| s.as_str()).unwrap_or("").to_string();
            let reply = hit.payload.get("reply").and_then(|v| v.as_str()).map(|s| s.as_str()).unwrap_or("").to_string();
            (context, reply)
        }).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to search style shots");
            Vec::new()
        }
    }
}
