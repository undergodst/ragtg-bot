use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    Condition, CreateCollectionBuilder, DeletePointsBuilder, Distance, Filter,
    PointStruct, PointsIdsList, SearchPointsBuilder, UpsertPointsBuilder,
    VectorParamsBuilder, Value as QdrantValue,
};
use std::collections::HashMap;

use crate::error::{Error, Result};

/// Vector dimension for BGE-M3 embeddings.
pub const VECTOR_DIM: u64 = 1024;

/// Collections used by the bot. Created on startup if missing.
pub const COLLECTIONS: &[&str] = &[
    "episodic_summaries",
    "user_facts",
    "chat_events",
    "media_descriptions",
    "style_shots",
];

pub fn init_client(url: &str) -> Result<Qdrant> {
    Qdrant::from_url(url)
        .build()
        .map_err(|e| Error::Qdrant(e.to_string()))
}

/// Idempotently create all required collections (vector dim 1024, Cosine, on_disk).
pub async fn ensure_collections(client: &Qdrant) -> Result<()> {
    for name in COLLECTIONS {
        let exists = client
            .collection_exists(*name)
            .await
            .map_err(|e| Error::Qdrant(format!("collection_exists({name}): {e}")))?;
        if exists {
            continue;
        }
        tracing::info!(collection = %name, "creating qdrant collection");
        let cfg = VectorParamsBuilder::new(VECTOR_DIM, Distance::Cosine).on_disk(true);
        client
            .create_collection(CreateCollectionBuilder::new(*name).vectors_config(cfg))
            .await
            .map_err(|e| Error::Qdrant(format!("create_collection({name}): {e}")))?;
    }
    Ok(())
}

/// Best-effort удаление коллекций, которые больше не входят в `COLLECTIONS`.
/// Для апгрейдов: бывшая `lore` коллекция может остаться от старой версии,
/// сносим её на старте чтобы не висела пустой и не фильтровала поиск
/// мусорно. Идемпотентно: если коллекции нет — `Ok(())`.
pub async fn cleanup_obsolete_collections(client: &Qdrant) -> Result<()> {
    const OBSOLETE: &[&str] = &["lore"];
    for name in OBSOLETE {
        match client.collection_exists(*name).await {
            Ok(true) => {
                tracing::info!(collection = %name, "deleting obsolete qdrant collection");
                if let Err(e) = client.delete_collection(*name).await {
                    tracing::warn!(collection = %name, error = %e, "delete obsolete failed (non-fatal)");
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(collection = %name, error = %e, "collection_exists failed (non-fatal)"),
        }
    }
    Ok(())
}

/// Verify Qdrant is reachable and all required collections exist.
pub async fn healthcheck(client: &Qdrant) -> Result<()> {
    let resp = client
        .list_collections()
        .await
        .map_err(|e| Error::Qdrant(format!("list_collections: {e}")))?;
    let names: Vec<&str> = resp.collections.iter().map(|c| c.name.as_str()).collect();
    for required in COLLECTIONS {
        if !names.contains(required) {
            return Err(Error::Qdrant(format!("missing collection: {required}")));
        }
    }
    Ok(())
}

/// A hit from a vector similarity search.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub score: f32,
    pub payload: HashMap<String, QdrantValue>,
}

/// Insert or update a point in a Qdrant collection.
pub async fn upsert_point(
    client: &Qdrant,
    collection: &str,
    point_id: &str,
    vector: Vec<f32>,
    payload: HashMap<String, QdrantValue>,
) -> Result<()> {
    let point = PointStruct::new(point_id.to_string(), vector, payload);

    client
        .upsert_points(UpsertPointsBuilder::new(collection, vec![point]).wait(true))
        .await
        .map_err(|e| Error::Qdrant(format!("upsert_point({collection}): {e}")))?;
    Ok(())
}

/// Search for the `top_k` most similar vectors in `collection`, filtered by
/// `chat_id`. Returns hits sorted by descending score.
pub async fn search_similar(
    client: &Qdrant,
    collection: &str,
    vector: Vec<f32>,
    chat_id: i64,
    top_k: u32,
) -> Result<Vec<SearchHit>> {
    let filter = Filter::must([Condition::matches("chat_id", chat_id)]);
    let results = client
        .search_points(
            SearchPointsBuilder::new(collection, vector, top_k as u64)
                .filter(filter)
                .with_payload(true),
        )
        .await
        .map_err(|e| Error::Qdrant(format!("search({collection}): {e}")))?;

    let hits = results
        .result
        .into_iter()
        .map(|sp| SearchHit {
            score: sp.score,
            payload: sp.payload,
        })
        .collect();
    Ok(hits)
}

/// Search the `user_facts` collection filtered by both `chat_id` and `user_id`.
pub async fn search_similar_user_facts(
    client: &Qdrant,
    vector: Vec<f32>,
    chat_id: i64,
    user_id: i64,
    top_k: u32,
) -> Result<Vec<SearchHit>> {
    let filter = Filter::must([
        Condition::matches("chat_id", chat_id),
        Condition::matches("user_id", user_id),
    ]);
    let results = client
        .search_points(
            SearchPointsBuilder::new("user_facts", vector, top_k as u64)
                .filter(filter)
                .with_payload(true),
        )
        .await
        .map_err(|e| Error::Qdrant(format!("search(user_facts): {e}")))?;

    let hits = results
        .result
        .into_iter()
        .map(|sp| SearchHit {
            score: sp.score,
            payload: sp.payload,
        })
        .collect();
    Ok(hits)
}

/// Delete a single point by its string ID.
pub async fn delete_point(
    client: &Qdrant,
    collection: &str,
    point_id: &str,
) -> Result<()> {
    let points = PointsIdsList {
        ids: vec![point_id.to_string().into()],
    };
    client
        .delete_points(
            DeletePointsBuilder::new(collection)
                .points(points)
                .wait(true),
        )
        .await
        .map_err(|e| Error::Qdrant(format!("delete_point({collection}): {e}")))?;
    Ok(())
}
