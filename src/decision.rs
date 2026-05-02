//! Decision layer: should the bot reply to this message?
//!
//! Two stages per CLAUDE.md:
//! 1. **Rule-based** (instant, no LLM): compute probability P from message
//!    properties, roll random number, skip if r > P.
//! 2. **LLM classifier** (only if stage 1 passed): ask a free model
//!    (DeepSeek V3 :free) whether the bot should react. "no" → skip.

use teloxide::prelude::*;
use teloxide::types::MessageEntityKind;

use crate::config::DecisionConfig;
use crate::deps::Deps;
use crate::llm::client::Message as LlmMessage;
use crate::llm::prompts::decision::DECISION_PROMPT;
use crate::memory::working;
use crate::metrics;

const DECISION_MAX_TOKENS: u32 = 10;

/// Full decision pipeline. Returns `true` if the bot should reply.
///
/// Called from `handle_message` INSTEAD of the old `should_reply` that only
/// checked @mention / reply-to-bot.
pub async fn should_reply(bot: &Bot, msg: &Message, deps: &Deps) -> anyhow::Result<bool> {
    // Message must have either text/caption OR some form of media.
    let text = msg.text().or_else(|| msg.caption());
    let has_media = crate::bot::handlers::detect_media(msg);
    
    if text.is_none() && !has_media {
        return Ok(false);
    }

    // Private chats: always reply (bot is the only interlocutor).
    if msg.chat.is_private() {
        metrics::DECISION_OUTCOMES.with_label_values(&["reply"]).inc();
        return Ok(true);
    }

    let bot_id = deps.bot_id;
    let bot_username = &deps.bot_username;
    let cfg = &deps.config.decision;

    // ── Stage 1: rule-based probability ──────────────────────────────
    let p = compute_probability(msg, bot_id, bot_username, &deps.config.bot.aliases, cfg, &deps.redis, msg.chat.id.0).await;

    // Deterministic cases (P=1.0) skip the random roll.
    if p < 1.0 {
        let r: f32 = rand_f32();
        if r > p {
            tracing::debug!(
                chat_id = msg.chat.id.0,
                p,
                r,
                "decision stage1: skipped (r > p)"
            );
            metrics::DECISION_OUTCOMES.with_label_values(&["skip_rule"]).inc();
            return Ok(false);
        }
    }

    // ── Stage 2: LLM classifier (only on probabilistic passes) ──────
    // Skip stage 2 for direct mentions / replies (P=1.0) to avoid
    // latency on messages explicitly directed at the bot.
    if p < 1.0 {
        match llm_classify(msg, deps).await {
            Ok(true) => { /* pass */ }
            Ok(false) => {
                tracing::debug!(
                    chat_id = msg.chat.id.0,
                    "decision stage2: LLM said no"
                );
                metrics::DECISION_OUTCOMES.with_label_values(&["skip_llm"]).inc();
                return Ok(false);
            }
            Err(e) => {
                // On LLM failure, let the message through — better to
                // occasionally over-reply than silently break.
                tracing::warn!(error = %e, "decision LLM failed; allowing reply");
                metrics::DECISION_OUTCOMES.with_label_values(&["error"]).inc();
            }
        }
    }

    metrics::DECISION_OUTCOMES.with_label_values(&["reply"]).inc();
    Ok(true)
}

/// Compute the reply probability based on message properties.
async fn compute_probability(
    msg: &Message,
    bot_id: i64,
    bot_username: &str,
    bot_aliases: &[String],
    cfg: &DecisionConfig,
    redis: &deadpool_redis::Pool,
    chat_id: i64,
) -> f32 {
    // Direct @mention → P = mention_p (default 1.0)
    if has_mention(msg, bot_username) {
        return cfg.mention_p;
    }

    // Reply to bot's message → P = reply_p (default 1.0)
    if let Some(reply_to) = msg.reply_to_message()
        && let Some(replied_user) = reply_to.from.as_ref()
        && replied_user.id.0 as i64 == bot_id
    {
        return cfg.reply_p;
    }

    // Bot name in text (without @mention) → P = name_in_text_p (default 0.7)
    if let Some(text) = msg.text().or_else(|| msg.caption()) {
        let lower = text.to_lowercase();
        let name_lower = bot_username.to_lowercase();
        
        // Check official username
        if lower.contains(&name_lower) {
            return cfg.name_in_text_p;
        }

        // Check custom aliases (like 'пидрила')
        for alias in bot_aliases {
            if lower.contains(&alias.to_lowercase()) {
                return cfg.name_in_text_p;
            }
        }

        // Text contains a question AND bot hasn't replied in >N minutes
        if contains_question(&lower) {
            if let Ok(last_bot_ts) = get_last_bot_reply_ts(redis, chat_id, bot_id).await {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let silence_sec = (cfg.silence_threshold_min as i64) * 60;
                if now - last_bot_ts > silence_sec {
                    return cfg.question_after_silence_p;
                }
            }
        }
    }

    // Default random participation
    cfg.random_p
}

/// Check if the message @mentions the bot.
fn has_mention(msg: &Message, bot_username: &str) -> bool {
    let entities = msg.parse_entities().or_else(|| msg.parse_caption_entities());
    let Some(entities) = entities else {
        return false;
    };
    let needle = format!("@{bot_username}");
    for ent in entities {
        if matches!(ent.kind(), MessageEntityKind::Mention)
            && ent.text().eq_ignore_ascii_case(&needle)
        {
            return true;
        }
    }
    false
}

/// Simple heuristic: does the text contain a question mark or common
/// Russian question starters?
fn contains_question(lower: &str) -> bool {
    lower.contains('?')
        || lower.starts_with("кто ")
        || lower.starts_with("что ")
        || lower.starts_with("как ")
        || lower.starts_with("где ")
        || lower.starts_with("когда ")
        || lower.starts_with("зачем ")
        || lower.starts_with("почему ")
        || lower.starts_with("сколько ")
}

/// Get the timestamp of the bot's last message in the working window.
async fn get_last_bot_reply_ts(
    redis: &deadpool_redis::Pool,
    chat_id: i64,
    bot_id: i64,
) -> anyhow::Result<i64> {
    let window = working::get_window(redis, chat_id, 30).await?;
    let ts = window
        .iter()
        .rev()
        .find(|m| m.user_id == bot_id)
        .map(|m| m.ts)
        .unwrap_or(0);
    Ok(ts)
}

/// Stage 2: ask a free LLM whether the bot should react.
async fn llm_classify(msg: &Message, deps: &Deps) -> anyhow::Result<bool> {
    let chat_id = msg.chat.id.0;

    // Get last 5 messages from working window for context.
    let window = working::get_window(&deps.redis, chat_id, 5)
        .await
        .unwrap_or_default();

    let mut context = String::new();
    if !window.is_empty() {
        context.push_str("Последние сообщения:\n");
        for w in &window {
            let who = w.username.as_deref().unwrap_or("anon");
            context.push_str(&format!("{who}: {}\n", truncate_text(&w.text, 150)));
        }
        context.push('\n');
    }

    let user_text = msg.text().or_else(|| msg.caption()).unwrap_or_default();
    let who = msg
        .from
        .as_ref()
        .and_then(|u| u.username.clone())
        .unwrap_or_else(|| "anon".into());
    context.push_str(&format!("Новое сообщение от {who}: {}", truncate_text(user_text, 300)));

    let messages = vec![
        LlmMessage::system(DECISION_PROMPT),
        LlmMessage::user(context),
    ];

    let model = &deps.config.openrouter.model_decision;
    let completion = deps
        .openrouter
        .chat_completion("decision", model, &messages, DECISION_MAX_TOKENS)
        .await?;

    let answer = completion.content.trim().to_lowercase();
    tracing::debug!(
        chat_id,
        model = %completion.model,
        latency_ms = completion.latency_ms,
        answer = %answer,
        "decision LLM"
    );

    Ok(answer.contains("yes"))
}

/// Simple random f32 in [0, 1) using timestamp-based seed.
/// Not cryptographically secure, but fine for reply probability.
fn rand_f32() -> f32 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    // XorShift-ish mixing of nanoseconds.
    let mut x = nanos;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    (x as f32) / (u32::MAX as f32)
}

fn truncate_text(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}
