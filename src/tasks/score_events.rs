//! Stage-2 LLM scorer for chat_events.
//!
//! Trigger: `maybe_score(deps, chat_id)` — called from the same fire-and-forget
//! background block as summary/facts. On every call:
//!   1. Cheap LLEN check; bail out if buffer hasn't reached
//!      `events.buffer_threshold`.
//!   2. Otherwise atomically pop the buffer, fetch a few messages of prior
//!      context, and ask a (free) LLM to score each candidate as
//!      `[{"i": 0, "score": 4, "category": "quote"}]`.
//!   3. Batch-embed surviving entries (one HTTP), then call the existing
//!      `events::insert` to write to SQLite + Qdrant.
//!
//! Failure modes:
//!   - All score models fail → re-queue candidates so the next trigger retries.
//!   - LLM returns garbage (unparseable JSON) → drop this batch (don't loop).
//!   - Embedding/insert fails for one entry → log + skip that one only.

use serde::Deserialize;

use crate::deps::Deps;
use crate::llm::client::Message as LlmMessage;
use crate::llm::prompts::score::SCORE_SYSTEM_PROMPT;
use crate::memory::events::{self, CandidateRow};
use crate::storage::redis as redis_store;

const SCORE_MAX_TOKENS: u32 = 800;
/// How many prior messages we feed the model as unscored context. Plain
/// number per spec §5.2.
const CONTEXT_MESSAGES: i64 = 5;
/// One of the seven canonical categories — used to validate LLM output.
fn is_valid_category(c: &str) -> bool {
    matches!(
        c,
        "quote" | "event" | "meme" | "conflict" | "fact" | "media" | "banger"
    )
}

#[derive(Debug, Deserialize)]
struct ScoredEntry {
    i: u32,
    score: u8,
    category: String,
}

struct ContextRow {
    username: Option<String>,
    text: Option<String>,
    media_description: Option<String>,
}

/// Entry point. Best-effort: returns `Ok(())` on success or "buffer not full
/// enough"; bubbles up only catastrophic infrastructure errors. Used inside
/// `tokio::spawn`, so callers don't await its result.
pub async fn maybe_score(deps: &Deps, chat_id: i64) -> anyhow::Result<()> {
    let threshold = deps.config.events.buffer_threshold as i64;
    if threshold <= 0 {
        return Ok(());
    }

    let len = redis_store::len_event_candidates(&deps.redis, chat_id).await?;
    if len < threshold {
        return Ok(());
    }

    tracing::info!(chat_id, buffer_len = len, "events scoring triggered");
    run_score(deps, chat_id).await
}

async fn run_score(deps: &Deps, chat_id: i64) -> anyhow::Result<()> {
    // Atomic pop — gives us full control of the batch and lets new
    // candidates accumulate while we score.
    let raw_payloads = redis_store::pop_event_candidates(&deps.redis, chat_id).await?;
    if raw_payloads.is_empty() {
        return Ok(());
    }

    // Filter parse-failures out. We never re-queue garbage payloads.
    let candidates: Vec<CandidateRow> = raw_payloads
        .iter()
        .filter_map(|s| match serde_json::from_str::<CandidateRow>(s) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "score: dropping unparseable candidate payload");
                None
            }
        })
        .collect();

    if candidates.is_empty() {
        return Ok(());
    }

    // Fetch a few prior messages as context (older than the oldest candidate).
    let oldest_sqlite_id = candidates
        .iter()
        .map(|c| c.sqlite_message_id)
        .min()
        .unwrap_or(i64::MAX);
    let context = fetch_context_before(&deps.sqlite, chat_id, oldest_sqlite_id, CONTEXT_MESSAGES)
        .await
        .unwrap_or_default();

    let user_block = format_score_payload(&context, &candidates);
    let prompt = vec![
        LlmMessage::system(SCORE_SYSTEM_PROMPT),
        LlmMessage::user(user_block),
    ];

    let mut models: Vec<&str> = Vec::with_capacity(1 + deps.config.events.score_fallbacks.len());
    models.push(&deps.config.events.score_model);
    models.extend(deps.config.events.score_fallbacks.iter().map(String::as_str));

    let completion = match try_score_with_fallback(deps, &models, &prompt).await {
        Some(c) => c,
        None => {
            tracing::warn!(
                chat_id,
                count = raw_payloads.len(),
                "all score models failed; re-queuing candidates"
            );
            redis_store::requeue_event_candidates(&deps.redis, chat_id, &raw_payloads).await?;
            return Ok(());
        }
    };

    let raw = completion.content.trim();
    tracing::info!(
        chat_id,
        model = %completion.model,
        latency_ms = completion.latency_ms,
        candidates = candidates.len(),
        "scorer LLM done"
    );

    let parsed: Vec<ScoredEntry> = match parse_score_json(raw) {
        Some(v) => v,
        None => {
            tracing::warn!(
                chat_id,
                raw = %truncate(raw, 200),
                "scorer returned unparseable JSON; dropping batch"
            );
            // Note: not requeueing — bad output is not transient.
            return Ok(());
        }
    };

    let score_min = deps.config.events.score_min;
    let mut kept: Vec<(CandidateRow, ScoredEntry)> = Vec::new();
    for entry in parsed.into_iter() {
        if entry.score < score_min {
            continue;
        }
        if !is_valid_category(&entry.category) {
            tracing::debug!(category = %entry.category, "score: ignoring unknown category");
            continue;
        }
        let Some(c) = candidates.get(entry.i as usize) else {
            tracing::debug!(idx = entry.i, "score: index out of range; ignoring");
            continue;
        };
        kept.push((c.clone(), entry));
    }

    let dropped = candidates.len().saturating_sub(kept.len());
    if dropped > 0 {
        crate::metrics::EVENTS_SCORED_TOTAL
            .with_label_values(&["dropped"])
            .inc_by(dropped as u64);
    }

    if kept.is_empty() {
        tracing::info!(chat_id, candidates = candidates.len(), "scorer kept 0 events");
        return Ok(());
    }

    let texts: Vec<String> = kept
        .iter()
        .map(|(c, _)| format_event_text(c))
        .collect();
    let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();

    let vectors = match deps.embeddings.embed(&text_refs).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "score: batch embedding failed; dropping kept");
            return Ok(());
        }
    };
    if vectors.len() != kept.len() {
        tracing::warn!(
            expected = kept.len(),
            got = vectors.len(),
            "score: embedding count mismatch; dropping batch"
        );
        return Ok(());
    }

    for ((cand, scored), vector) in kept.into_iter().zip(vectors.into_iter()) {
        let text = format_event_text(&cand);
        match events::insert(
            deps,
            chat_id,
            Some(cand.sqlite_message_id),
            &text,
            &scored.category,
            scored.score,
            vector,
        )
        .await
        {
            Ok(_) => {
                crate::metrics::EVENTS_SCORED_TOTAL
                    .with_label_values(&["kept"])
                    .inc();
            }
            Err(e) => {
                tracing::warn!(error = %e, "score: events::insert failed");
            }
        }
    }

    Ok(())
}

async fn try_score_with_fallback(
    deps: &Deps,
    models: &[&str],
    prompt: &[LlmMessage],
) -> Option<crate::llm::client::ChatCompletion> {
    for model in models {
        match deps
            .openrouter
            .chat_completion("score", model, prompt, SCORE_MAX_TOKENS)
            .await
        {
            Ok(c) => {
                if c.content.trim().is_empty() {
                    tracing::warn!(model, "score: empty content; trying next");
                    continue;
                }
                return Some(c);
            }
            Err(e) => {
                tracing::warn!(model, error = %e, "score model failed; trying next");
            }
        }
    }
    None
}

fn format_score_payload(context: &[ContextRow], candidates: &[CandidateRow]) -> String {
    let mut s = String::new();
    if !context.is_empty() {
        s.push_str("КОНТЕКСТ (без оценки):\n");
        for c in context {
            let who = c.username.as_deref().unwrap_or("anon");
            let text = c.text.as_deref().unwrap_or("").trim();
            s.push_str(&format!("- {who}: {text}"));
            if let Some(d) = c.media_description.as_deref() {
                s.push_str(&format!(" [медиа: {}]", d.trim()));
            }
            s.push('\n');
        }
        s.push('\n');
    }
    s.push_str("КАНДИДАТЫ:\n");
    for (i, c) in candidates.iter().enumerate() {
        let who = c.username.as_deref().unwrap_or("anon");
        s.push_str(&format!("[i={i}] {who}: {}", c.text.trim()));
        if let Some(d) = c.media_desc.as_deref() {
            s.push_str(&format!(" [медиа: {}]", d.trim()));
        }
        s.push('\n');
    }
    s
}

fn format_event_text(c: &CandidateRow) -> String {
    let who = c.username.as_deref().unwrap_or("anon");
    let mut s = format!("{who}: {}", c.text.trim());
    if let Some(d) = c.media_desc.as_deref() {
        s.push_str(&format!(" [медиа: {}]", d.trim()));
    }
    s
}

async fn fetch_context_before(
    pool: &sqlx::SqlitePool,
    chat_id: i64,
    before_sqlite_id: i64,
    limit: i64,
) -> anyhow::Result<Vec<ContextRow>> {
    let rows = sqlx::query!(
        r#"SELECT u.username, m.text, m.media_description
           FROM messages m
           LEFT JOIN users u ON u.id = m.user_id
           WHERE m.chat_id = ? AND m.id < ?
           ORDER BY m.id DESC
           LIMIT ?"#,
        chat_id,
        before_sqlite_id,
        limit,
    )
    .fetch_all(pool)
    .await?;

    // Reverse to chronological for the prompt.
    let mut out: Vec<ContextRow> = rows
        .into_iter()
        .map(|r| ContextRow {
            username: r.username,
            text: r.text,
            media_description: r.media_description,
        })
        .collect();
    out.reverse();
    Ok(out)
}

fn parse_score_json(raw: &str) -> Option<Vec<ScoredEntry>> {
    if let Ok(v) = serde_json::from_str::<Vec<ScoredEntry>>(raw) {
        return Some(v);
    }
    let cleaned = strip_code_block(raw);
    serde_json::from_str::<Vec<ScoredEntry>>(&cleaned).ok()
}

fn strip_code_block(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix("```json").unwrap_or(t);
    let t = t.strip_prefix("```").unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t);
    t.trim().to_string()
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}
