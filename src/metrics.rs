//! Prometheus metrics for the bot.
//!
//! All metrics are registered with the default global registry and
//! exposed via the `/metrics` endpoint in main.rs.

use prometheus::{
    HistogramOpts, HistogramVec, IntCounter, IntCounterVec, Opts, register_histogram_vec,
    register_int_counter, register_int_counter_vec,
};
use std::sync::LazyLock;

/// Total messages received by the bot (all chats).
pub static MESSAGES_RECEIVED: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_messages_received_total", "Total messages received")
    )
    .expect("register bot_messages_received_total")
});

/// Messages that passed the decision layer and got a reply.
pub static REPLIES_SENT: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_replies_sent_total", "Total replies sent by the bot")
    )
    .expect("register bot_replies_sent_total")
});

/// Decision layer outcomes.
pub static DECISION_OUTCOMES: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        Opts::new("bot_decision_total", "Decision layer outcomes"),
        &["result"] // "reply", "skip_rule", "skip_llm", "error"
    )
    .expect("register bot_decision_total")
});

/// LLM call latency (seconds), by purpose.
pub static LLM_LATENCY: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        HistogramOpts::new("bot_llm_latency_seconds", "LLM call latency")
            .buckets(vec![0.5, 1.0, 2.0, 3.0, 5.0, 8.0, 15.0, 30.0]),
        &["purpose"] // "reply", "decision", "summary", "facts", "perception", "embedding"
    )
    .expect("register bot_llm_latency_seconds")
});

/// LLM calls total, by purpose and status.
pub static LLM_CALLS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        Opts::new("bot_llm_calls_total", "LLM API calls"),
        &["purpose", "status"] // status: "ok", "error"
    )
    .expect("register bot_llm_calls_total")
});

/// Episodic summarizations completed.
pub static SUMMARIES_CREATED: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_summaries_created_total", "Episodic summaries created")
    )
    .expect("register bot_summaries_created_total")
});

/// Facts extracted (total across all users).
pub static FACTS_EXTRACTED: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_facts_extracted_total", "User facts extracted")
    )
    .expect("register bot_facts_extracted_total")
});

/// Rate-limited messages (dropped silently).
pub static RATE_LIMITED: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_rate_limited_total", "Messages dropped by rate limiting")
    )
    .expect("register bot_rate_limited_total")
});

/// Chat events successfully stored (Phase 3 scorer will drive this).
pub static EVENTS_STORED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_events_stored_total", "chat_events successfully stored")
    )
    .expect("register bot_events_stored_total")
});

/// Heuristic Stage-1 candidates that survived `is_candidate` and got
/// pushed into the Redis buffer.
pub static EVENTS_CANDIDATE_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_events_candidate_total", "Candidates that passed Stage-1 heuristic")
    )
    .expect("register bot_events_candidate_total")
});

/// Stage-2 LLM scoring outcomes per candidate batch entry.
pub static EVENTS_SCORED_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        Opts::new("bot_events_scored_total", "Candidates after LLM scoring"),
        &["outcome"] // "kept", "dropped"
    )
    .expect("register bot_events_scored_total")
});

/// Events removed by the daily Stage-3 dedup pass.
pub static EVENTS_DEDUP_REMOVED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_events_dedup_removed_total", "Events deleted by dedup task")
    )
    .expect("register bot_events_dedup_removed_total")
});

/// Successful vision/audio descriptions returned to the bot.
pub static VISION_DESCRIBE_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_vision_describe_total", "Successful vision/audio describe calls")
    )
    .expect("register bot_vision_describe_total")
});

/// Media SHA256 cache hits — perception was skipped because we already had a description.
pub static VISION_CACHE_HIT_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        Opts::new("bot_vision_cache_hit_total", "Media perception cache hits")
    )
    .expect("register bot_vision_cache_hit_total")
});

/// Which vision model actually produced the description (primary or any fallback).
pub static VISION_MODEL_USED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        Opts::new("bot_vision_model_used_total", "Vision model that produced the description"),
        &["model"]
    )
    .expect("register bot_vision_model_used_total")
});

/// Force-initialize all metrics so they appear in /metrics even before
/// the first event. Call once at startup.
pub fn init() {
    LazyLock::force(&MESSAGES_RECEIVED);
    LazyLock::force(&REPLIES_SENT);
    LazyLock::force(&DECISION_OUTCOMES);
    LazyLock::force(&LLM_LATENCY);
    LazyLock::force(&LLM_CALLS);
    LazyLock::force(&SUMMARIES_CREATED);
    LazyLock::force(&FACTS_EXTRACTED);
    LazyLock::force(&RATE_LIMITED);
    LazyLock::force(&EVENTS_STORED_TOTAL);
    LazyLock::force(&EVENTS_CANDIDATE_TOTAL);
    LazyLock::force(&EVENTS_SCORED_TOTAL);
    LazyLock::force(&EVENTS_DEDUP_REMOVED_TOTAL);
    LazyLock::force(&VISION_DESCRIBE_TOTAL);
    LazyLock::force(&VISION_CACHE_HIT_TOTAL);
    LazyLock::force(&VISION_MODEL_USED);
}
