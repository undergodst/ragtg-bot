use qdrant_client::Qdrant;
use qdrant_client::qdrant::{CreateCollectionBuilder, Distance, VectorParamsBuilder};

use crate::error::{Error, Result};

/// Vector dimension for BGE-M3 embeddings.
pub const VECTOR_DIM: u64 = 1024;

/// Collections used by the bot. Created on startup if missing.
pub const COLLECTIONS: &[&str] = &[
    "episodic_summaries",
    "user_facts",
    "lore",
    "media_descriptions",
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
