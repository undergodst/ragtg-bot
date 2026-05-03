//! Ступень 3 — фоновый дедуп.
//! На активный чат: последние 1000 событий.
//! Группировка по category. Внутри группы — попарное сравнение через Qdrant;
//! similarity > config.events.dedup_threshold → оставляем выше score
//! (при равенстве — старшее по created_at). Лишние удаляются и из SQLite, и из Qdrant.

use std::collections::{HashMap, HashSet};
use qdrant_client::qdrant::{GetPointsBuilder, PointId};

use crate::deps::Deps;

#[derive(Debug)]
struct EventRow {
    sqlite_id: i64,
    qdrant_point_id: String,
    score: u8,
    category: String,
    created_at: i64,
}

pub async fn run_dedup(deps: &Deps) -> anyhow::Result<()> {
    tracing::info!("starting global dedup task");
    
    // Find active chats in the last 24h
    let active_chats = sqlx::query_scalar!(
        r#"SELECT DISTINCT chat_id FROM messages WHERE created_at >= unixepoch() - 86400"#
    )
    .fetch_all(&deps.sqlite)
    .await?;

    for chat_id in active_chats {
        if let Err(e) = dedup_chat(deps, chat_id).await {
            tracing::warn!(error = %e, chat_id, "dedup for chat failed");
        }
    }

    Ok(())
}

async fn dedup_chat(deps: &Deps, chat_id: i64) -> anyhow::Result<()> {
    let limit = 1000;
    
    // Fetch last N events for this chat
    let rows = sqlx::query!(
        r#"SELECT id, qdrant_point_id, score, category, created_at 
           FROM chat_events 
           WHERE chat_id = ? 
           ORDER BY created_at DESC 
           LIMIT ?"#,
        chat_id,
        limit
    )
    .fetch_all(&deps.sqlite)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let mut by_category: HashMap<String, Vec<EventRow>> = HashMap::new();
    for r in rows {
        by_category.entry(r.category.clone()).or_default().push(EventRow {
            sqlite_id: r.id.unwrap(),
            qdrant_point_id: r.qdrant_point_id,
            score: r.score as u8,
            category: r.category,
            created_at: r.created_at,
        });
    }

    let threshold = deps.config.events.dedup_threshold;
    let mut dropped = 0;

    for (cat, events) in by_category {
        if events.len() < 2 {
            continue;
        }

        // Fetch vectors for this category from Qdrant in one batch
        let point_ids: Vec<PointId> = events.iter().map(|e| e.qdrant_point_id.clone().into()).collect();
        let qdrant_res = deps.qdrant.get_points(
            GetPointsBuilder::new("chat_events", point_ids).with_vectors(true)
        ).await;

        let points = match qdrant_res {
            Ok(p) => p.result,
            Err(e) => {
                tracing::warn!(error = %e, chat_id, category = cat, "failed to get vectors from qdrant");
                continue;
            }
        };

        // Map qdrant_point_id -> vector
        let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
        for p in points {
            if let Some(id) = p.id {
                let id_str = match id.point_id_options {
                    Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => u,
                    _ => continue,
                };
                if let Some(v) = p.vectors {
                    // Extract default vector
                    if let Some(qdrant_client::qdrant::vectors_output::VectorsOptions::Vector(vec)) = v.vectors_options {
                        vectors.insert(id_str, vec.data);
                    }
                }
            }
        }

        // NxN comparison
        let mut to_delete = HashSet::new();

        for i in 0..events.len() {
            let e1 = &events[i];
            if to_delete.contains(&e1.sqlite_id) {
                continue;
            }
            let v1 = match vectors.get(&e1.qdrant_point_id) {
                Some(v) => v,
                None => continue,
            };

            for j in (i + 1)..events.len() {
                let e2 = &events[j];
                if to_delete.contains(&e2.sqlite_id) {
                    continue;
                }
                let v2 = match vectors.get(&e2.qdrant_point_id) {
                    Some(v) => v,
                    None => continue,
                };

                let sim = cosine_similarity(v1, v2);
                if sim > threshold {
                    // They are duplicates! Keep the one with higher score, or older
                    if e1.score > e2.score {
                        to_delete.insert(e2.sqlite_id);
                    } else if e2.score > e1.score {
                        to_delete.insert(e1.sqlite_id);
                    } else {
                        // Same score, keep older (e1 is newer because ORDER BY created_at DESC)
                        if e1.created_at < e2.created_at {
                            to_delete.insert(e2.sqlite_id);
                        } else {
                            to_delete.insert(e1.sqlite_id);
                        }
                    }
                }
            }
        }

        for (sqlite_id, point_id) in events.iter().filter_map(|e| {
            if to_delete.contains(&e.sqlite_id) {
                Some((e.sqlite_id, e.qdrant_point_id.clone()))
            } else {
                None
            }
        }) {
            // Delete from SQLite
            sqlx::query!("DELETE FROM chat_events WHERE id = ?", sqlite_id)
                .execute(&deps.sqlite)
                .await?;
            
            // Delete from Qdrant
            crate::storage::qdrant::delete_point(&deps.qdrant, "chat_events", &point_id).await?;
            
            crate::metrics::EVENTS_DEDUP_REMOVED_TOTAL.inc();
            dropped += 1;
        }
    }

    if dropped > 0 {
        tracing::info!(chat_id, dropped, "chat events dedup completed");
    }

    Ok(())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (va, vb) in a.iter().zip(b.iter()) {
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}
