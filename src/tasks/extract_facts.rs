//! Background fact-extraction task.
//!
//! Entry point: `maybe_extract_facts(deps, chat_id, user_id)` — called
//! after every persisted message. Increments a per-user Redis counter;
//! when it hits `config.memory.facts_extraction_every_n`, pulls the last
//! N messages of that user, asks DeepSeek Flash to extract facts as JSON,
//! deduplicates via embedding similarity, and stores new facts in SQLite
//! + Qdrant.

use std::collections::HashMap;

use qdrant_client::qdrant::Value as QdrantValue;
use serde::Deserialize;
use uuid::Uuid;

use crate::deps::Deps;
use crate::llm::client::Message as LlmMessage;
use crate::llm::prompts::facts::FACTS_PROMPT;

use crate::storage::{qdrant as qdrant_store, redis as redis_store};

const FACTS_MAX_TOKENS: u32 = 500;

/// One fact as returned by the LLM.
#[derive(Debug, Deserialize)]
struct ExtractedFact {
    fact: String,
    fact_type: Option<String>,
}

/// Row returned by the "fetch user messages" query.
struct UserMessageRow {
    text: Option<String>,
    media_description: Option<String>,
}

/// Call this after every persisted message. Cheap (one Redis INCR) on
/// most calls; only does real work once every `facts_extraction_every_n`
/// messages per user.
pub async fn maybe_extract_facts(
    deps: &Deps,
    chat_id: i64,
    user_id: i64,
) -> anyhow::Result<()> {
    let threshold = deps.config.memory.facts_extraction_every_n as i64;
    if threshold <= 0 {
        return Ok(());
    }

    let count = redis_store::incr_facts_counter(&deps.redis, chat_id, user_id).await?;
    if count < threshold {
        return Ok(());
    }

    // Reset FIRST (same crash-safety logic as episodic summarization).
    redis_store::reset_facts_counter(&deps.redis, chat_id, user_id).await?;

    tracing::info!(chat_id, user_id, count, "facts extraction triggered");
    run_extract(deps, chat_id, user_id).await
}

async fn run_extract(deps: &Deps, chat_id: i64, user_id: i64) -> anyhow::Result<()> {
    let lookback = deps.config.memory.facts_lookback as i64;
    let messages = fetch_user_messages(&deps.sqlite, chat_id, user_id, lookback).await?;
    if messages.is_empty() {
        tracing::warn!(chat_id, user_id, "no messages to extract facts from");
        return Ok(());
    }

    // Look up username for context in the prompt.
    let username = sqlx::query_scalar!(
        r#"SELECT username FROM users WHERE id = ?"#,
        user_id
    )
    .fetch_optional(&deps.sqlite)
    .await?
    .flatten()
    .unwrap_or_else(|| format!("uid:{user_id}"));

    let formatted = format_user_messages(&username, &messages);
    let prompt_messages = vec![
        LlmMessage::system(FACTS_PROMPT),
        LlmMessage::user(formatted),
    ];

    let model = deps.config.openrouter.model_main.clone();
    let completion = deps
        .openrouter
        .chat_completion("facts", &model, &prompt_messages, FACTS_MAX_TOKENS)
        .await?;

    let raw_json = completion.content.trim();
    tracing::info!(
        chat_id,
        user_id,
        model = %completion.model,
        latency_ms = completion.latency_ms,
        "facts extraction LLM done"
    );

    // Parse JSON array of facts.
    let extracted: Vec<ExtractedFact> = match serde_json::from_str(raw_json) {
        Ok(v) => v,
        Err(e) => {
            // Try to extract JSON from markdown code blocks (```json ... ```)
            let cleaned = strip_code_block(raw_json);
            match serde_json::from_str(&cleaned) {
                Ok(v) => v,
                Err(_) => {
                    tracing::warn!(
                        chat_id,
                        user_id,
                        error = %e,
                        raw = %truncate(raw_json, 200),
                        "failed to parse facts JSON"
                    );
                    return Ok(());
                }
            }
        }
    };

    if extracted.is_empty() {
        tracing::info!(chat_id, user_id, "LLM returned no facts");
        return Ok(());
    }

    let dedup_threshold = deps.config.memory.fact_dedup_threshold;
    let mut stored = 0u32;

    for ef in &extracted {
        if ef.fact.trim().is_empty() {
            continue;
        }

        // Embed the new fact.
        let vector = match deps.embeddings.embed_single(&ef.fact).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "failed to embed fact; skipping");
                continue;
            }
        };

        // Dedup: search existing facts for this user in this chat.
        let hits = qdrant_store::search_similar_user_facts(
            &deps.qdrant,
            vector.clone(),
            chat_id,
            user_id,
            3,
        )
        .await
        .unwrap_or_default();

        let is_duplicate = hits.iter().any(|h| h.score >= dedup_threshold);
        if is_duplicate {
            tracing::debug!(
                chat_id,
                user_id,
                fact = %truncate(&ef.fact, 80),
                "fact deduplicated (similarity >= threshold)"
            );
            continue;
        }

        // Store new fact.
        let point_id = Uuid::new_v4().to_string();
        let fact_type = ef.fact_type.as_deref().unwrap_or("other");
        let confidence: f64 = 0.7;

        sqlx::query!(
            r#"INSERT INTO user_facts (user_id, chat_id, fact, fact_type, confidence, qdrant_point_id)
               VALUES (?, ?, ?, ?, ?, ?)"#,
            user_id,
            chat_id,
            ef.fact,
            fact_type,
            confidence,
            point_id
        )
        .execute(&deps.sqlite)
        .await?;

        let sqlite_id = sqlx::query_scalar!(
            r#"SELECT id AS "id!: i64" FROM user_facts
               WHERE qdrant_point_id = ? LIMIT 1"#,
            point_id
        )
        .fetch_one(&deps.sqlite)
        .await?;

        let mut payload: HashMap<String, QdrantValue> = HashMap::new();
        payload.insert("user_id".into(), QdrantValue::from(user_id));
        payload.insert("chat_id".into(), QdrantValue::from(chat_id));
        payload.insert("sqlite_id".into(), QdrantValue::from(sqlite_id));
        payload.insert("fact_type".into(), QdrantValue::from(fact_type.to_string()));

        qdrant_store::upsert_point(&deps.qdrant, "user_facts", &point_id, vector, payload)
            .await?;

        stored += 1;
    }

    tracing::info!(
        chat_id,
        user_id,
        extracted = extracted.len(),
        stored,
        "facts extraction complete"
    );

    Ok(())
}

async fn fetch_user_messages(
    pool: &sqlx::SqlitePool,
    chat_id: i64,
    user_id: i64,
    limit: i64,
) -> anyhow::Result<Vec<UserMessageRow>> {
    let rows = sqlx::query_as!(
        UserMessageRow,
        r#"SELECT text, media_description
           FROM messages
           WHERE chat_id = ? AND user_id = ?
           ORDER BY created_at DESC
           LIMIT ?"#,
        chat_id,
        user_id,
        limit
    )
    .fetch_all(pool)
    .await?;

    let mut rows = rows;
    rows.reverse();
    Ok(rows)
}

fn format_user_messages(username: &str, messages: &[UserMessageRow]) -> String {
    let mut out = format!("Сообщения пользователя @{username}:\n\n");
    for m in messages {
        let text = m.text.as_deref().unwrap_or("");
        if let Some(desc) = &m.media_description {
            out.push_str(&format!("{text} [{desc}]\n"));
        } else if !text.is_empty() {
            out.push_str(&format!("{text}\n"));
        }
    }
    out
}

/// Strip markdown code block wrappers if present (```json ... ``` or ``` ... ```).
fn strip_code_block(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.trim().strip_suffix("```").unwrap_or(rest.trim()).to_string()
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.trim().strip_suffix("```").unwrap_or(rest.trim()).to_string()
    } else {
        trimmed.to_string()
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}
