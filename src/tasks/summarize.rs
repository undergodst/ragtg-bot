//! Background episodic-summarisation task.
//!
//! Entry point: `maybe_summarize(deps, chat_id)` — called after every
//! persisted message.  Increments a Redis counter; when it hits
//! `config.memory.episodic_summary_every_n`, pulls the last N messages
//! from SQLite, summarises via DeepSeek Flash, embeds the summary, and
//! stores the result in both SQLite and Qdrant.

use std::collections::HashMap;

use qdrant_client::qdrant::Value as QdrantValue;
use uuid::Uuid;

use crate::deps::Deps;
use crate::llm::client::Message as LlmMessage;
use crate::llm::prompts::summary::SUMMARY_PROMPT;
use crate::storage::{qdrant as qdrant_store, redis as redis_store};

const SUMMARY_MAX_TOKENS: u32 = 300;

/// Row returned by the "fetch recent messages" query.
struct MessageRow {
    user_id: i64,
    username: Option<String>,
    text: Option<String>,
    media_description: Option<String>,
}

/// Call this after every persisted message.  Cheap (one Redis INCR) on
/// most calls; only does real work once every `episodic_summary_every_n`
/// messages.
pub async fn maybe_summarize(deps: &Deps, chat_id: i64) -> anyhow::Result<()> {
    let threshold = deps.config.memory.episodic_summary_every_n as i64;
    if threshold <= 0 {
        return Ok(());
    }

    let count = redis_store::incr_episodic_counter(&deps.redis, chat_id).await?;
    if count < threshold {
        return Ok(());
    }

    // Reset FIRST so a crash mid-summarise doesn't stall the counter forever
    // (we just lose one summary — the next N messages will trigger another).
    redis_store::reset_episodic_counter(&deps.redis, chat_id).await?;

    tracing::info!(chat_id, count, "episodic summary triggered");
    run_summarize(deps, chat_id).await
}

async fn run_summarize(deps: &Deps, chat_id: i64) -> anyhow::Result<()> {
    let lookback = deps.config.memory.episodic_summary_lookback as i64;
    let messages = fetch_recent_messages(&deps.sqlite, chat_id, lookback).await?;
    if messages.is_empty() {
        tracing::warn!(chat_id, "no messages to summarize");
        return Ok(());
    }

    let formatted = format_messages_for_summary(&messages);
    let prompt_messages = vec![
        LlmMessage::system(SUMMARY_PROMPT),
        LlmMessage::user(formatted),
    ];

    let model = deps.config.openrouter.model_main.clone();
    let completion = deps
        .openrouter
        .chat_completion(&model, &prompt_messages, SUMMARY_MAX_TOKENS)
        .await?;

    let summary_text = completion.content.trim().to_string();
    if summary_text.is_empty() {
        tracing::warn!(chat_id, "LLM returned empty summary");
        return Ok(());
    }

    tracing::info!(
        chat_id,
        model = %completion.model,
        latency_ms = completion.latency_ms,
        summary_len = summary_text.len(),
        "episodic summary generated"
    );

    // Embed the summary text.
    let vector = deps.embeddings.embed_single(&summary_text).await?;

    // Generate a UUID for the Qdrant point.
    let point_id = Uuid::new_v4().to_string();

    // Insert into SQLite.
    let qdrant_point_id_clone = point_id.clone();
    let range_start_i64: Option<i64> = None; // We'll store message range later when we have proper IDs
    let range_end_i64: Option<i64> = None;
    sqlx::query!(
        r#"INSERT INTO episodic_summaries (chat_id, text, qdrant_point_id, message_range_start, message_range_end)
           VALUES (?, ?, ?, ?, ?)"#,
        chat_id,
        summary_text,
        qdrant_point_id_clone,
        range_start_i64,
        range_end_i64
    )
    .execute(&deps.sqlite)
    .await?;

    let sqlite_id = sqlx::query_scalar!(
        r#"SELECT id AS "id!: i64" FROM episodic_summaries
           WHERE qdrant_point_id = ? LIMIT 1"#,
        qdrant_point_id_clone
    )
    .fetch_one(&deps.sqlite)
    .await?;

    // Build Qdrant payload.
    let mut payload: HashMap<String, QdrantValue> = HashMap::new();
    payload.insert("chat_id".into(), QdrantValue::from(chat_id));
    payload.insert("sqlite_id".into(), QdrantValue::from(sqlite_id));
    payload.insert("text".into(), QdrantValue::from(summary_text.clone()));

    // Upsert into Qdrant.
    qdrant_store::upsert_point(&deps.qdrant, "episodic_summaries", &point_id, vector, payload)
        .await?;

    tracing::info!(
        chat_id,
        sqlite_id,
        point_id = %point_id,
        "episodic summary stored"
    );

    Ok(())
}

async fn fetch_recent_messages(
    pool: &sqlx::SqlitePool,
    chat_id: i64,
    limit: i64,
) -> anyhow::Result<Vec<MessageRow>> {
    // Fetch the most recent messages for this chat, ordered oldest-first
    // (so the summary reads chronologically).
    let rows = sqlx::query_as!(
        MessageRow,
        r#"SELECT
             m.user_id AS "user_id!: i64",
             u.username,
             m.text,
             m.media_description
           FROM messages m
           JOIN users u ON u.id = m.user_id
           WHERE m.chat_id = ?
           ORDER BY m.created_at DESC
           LIMIT ?"#,
        chat_id,
        limit
    )
    .fetch_all(pool)
    .await?;

    // Reverse to chronological order (query returns newest-first for LIMIT).
    let mut rows = rows;
    rows.reverse();
    Ok(rows)
}

fn format_messages_for_summary(messages: &[MessageRow]) -> String {
    let mut out = String::with_capacity(messages.len() * 80);
    for m in messages {
        let who = m
            .username
            .as_deref()
            .unwrap_or("anon");
        let text = m.text.as_deref().unwrap_or("");
        if let Some(desc) = &m.media_description {
            out.push_str(&format!("{who}: {text} [{desc}]\n"));
        } else if !text.is_empty() {
            out.push_str(&format!("{who}: {text}\n"));
        }
    }
    out
}
