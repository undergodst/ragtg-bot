use teloxide::prelude::*;
use teloxide::types::{Chat, MessageEntityKind, MessageId, ReplyParameters};

use crate::deps::Deps;
use crate::llm::client::Message as LlmMessage;
use crate::llm::prompts::system::SYSTEM_PROMPT_BASE;
use crate::memory::working::{self, WorkingMessage};
use crate::storage::redis as rl;

const TEXT_PREVIEW_LEN: usize = 100;
const TEXT_MAX_LEN: usize = 8000;
const REPLY_MAX_TOKENS: u32 = 300;

/// Persist every incoming message that has a `from` user, then maybe reply
/// (if the bot was mentioned or replied to). Errors are logged but not
/// propagated — losing a row of stats or a single LLM call isn't worth
/// crashing the dispatcher.
pub async fn handle_message(bot: Bot, msg: Message, deps: Deps) -> ResponseResult<()> {
    if let Err(e) = save_message(&msg, &deps).await {
        tracing::warn!(error = %e, chat_id = msg.chat.id.0, "failed to persist message");
    }

    match should_reply(&bot, &msg).await {
        Ok(true) => {
            match rate_limit_pass(&msg, &deps).await {
                Ok(true) => {
                    if let Err(e) = reply(&bot, &msg, &deps).await {
                        tracing::warn!(error = %e, chat_id = msg.chat.id.0, "failed to reply");
                    }
                }
                Ok(false) => {
                    // Silent skip per CLAUDE.md: don't tell the user they're throttled.
                }
                Err(e) => {
                    tracing::warn!(error = %e, "rate-limit check failed; skipping reply");
                }
            }
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "should_reply check failed"),
    }
    Ok(())
}

/// Check both gates: per-user cooldown (per CLAUDE.md `ratelimit.user_cooldown_sec`)
/// and per-chat quota (`ratelimit.chat_max_per_min`). User cooldown is checked
/// FIRST and short-circuits — a hammering user mustn't burn the whole chat's
/// minute budget. We only consume the chat quota slot once the user passed.
/// Returns `Ok(false)` to mean "drop this reply silently".
async fn rate_limit_pass(msg: &Message, deps: &Deps) -> anyhow::Result<bool> {
    let chat_id = msg.chat.id.0;
    let cfg = &deps.config.ratelimit;

    if let Some(user) = msg.from.as_ref() {
        let user_id = user.id.0 as i64;
        if !rl::check_user_cooldown(&deps.redis, user_id, cfg.user_cooldown_sec).await? {
            tracing::info!(chat_id, user_id, "rate-limited by user cooldown");
            return Ok(false);
        }
    }

    if !rl::check_chat_quota(&deps.redis, chat_id, cfg.chat_max_per_min).await? {
        tracing::info!(chat_id, "rate-limited by chat quota");
        return Ok(false);
    }
    Ok(true)
}

async fn save_message(msg: &Message, deps: &Deps) -> anyhow::Result<()> {
    let Some(user) = msg.from.as_ref() else {
        // Channel post / anonymous group admin / etc. Skip for now.
        return Ok(());
    };

    let chat_id = msg.chat.id.0;
    let user_id = user.id.0 as i64;
    let username = user.username.clone();
    let first_name = user.first_name.clone();
    let chat_title = chat_label(&msg.chat);
    let tg_message_id = msg.id.0 as i64;
    let text = clip_text(extract_text(msg));
    let has_media: i64 = if detect_media(msg) { 1 } else { 0 };

    sqlx::query!(
        r#"INSERT INTO users (id, username, first_name)
           VALUES (?, ?, ?)
           ON CONFLICT(id) DO UPDATE SET
             username = excluded.username,
             first_name = excluded.first_name"#,
        user_id,
        username,
        first_name
    )
    .execute(&deps.sqlite)
    .await?;

    sqlx::query!(
        r#"INSERT INTO chats (id, title)
           VALUES (?, ?)
           ON CONFLICT(id) DO UPDATE SET title = excluded.title"#,
        chat_id,
        chat_title
    )
    .execute(&deps.sqlite)
    .await?;

    sqlx::query!(
        r#"INSERT INTO messages (chat_id, user_id, tg_message_id, text, has_media)
           VALUES (?, ?, ?, ?, ?)"#,
        chat_id,
        user_id,
        tg_message_id,
        text,
        has_media
    )
    .execute(&deps.sqlite)
    .await?;

    let preview: String = text
        .as_deref()
        .unwrap_or("")
        .chars()
        .take(TEXT_PREVIEW_LEN)
        .collect();
    tracing::info!(
        chat_id,
        user_id,
        tg_message_id,
        has_media = has_media == 1,
        msg = %preview,
        "msg saved"
    );

    if let Some(t) = text.clone() {
        let entry = WorkingMessage {
            user_id,
            username: username.clone(),
            text: t,
            media_desc: None,
            ts: msg.date.timestamp(),
        };
        working::push(
            &deps.redis,
            chat_id,
            &entry,
            deps.config.memory.working_window_size,
            deps.config.memory.working_ttl_days,
        )
        .await?;
    }

    Ok(())
}

/// Decide whether to answer this message: yes if the bot is @mentioned in
/// it, or if the user replied to a bot message. Everything else is silence
/// for now — the proper decision layer (rule + LLM) lands in ШАГ 8.
async fn should_reply(bot: &Bot, msg: &Message) -> anyhow::Result<bool> {
    if extract_text(msg).is_none() {
        return Ok(false);
    }

    // Private chats: бот = единственный собеседник, всегда отвечаем.
    if msg.chat.is_private() {
        return Ok(true);
    }

    let me = bot.get_me().await?;
    let bot_id = me.id.0 as i64;
    let bot_username = me.username().to_string();

    if let Some(reply_to) = msg.reply_to_message()
        && let Some(replied_user) = reply_to.from.as_ref()
        && replied_user.id.0 as i64 == bot_id
    {
        return Ok(true);
    }

    if has_mention(msg, &bot_username) {
        return Ok(true);
    }
    Ok(false)
}

fn has_mention(msg: &Message, bot_username: &str) -> bool {
    let entities = msg.parse_entities().or_else(|| msg.parse_caption_entities());
    let Some(entities) = entities else {
        return false;
    };
    let needle = format!("@{bot_username}");
    for ent in entities {
        if matches!(ent.kind(), MessageEntityKind::Mention) && ent.text().eq_ignore_ascii_case(&needle) {
            return true;
        }
    }
    false
}

async fn reply(bot: &Bot, msg: &Message, deps: &Deps) -> anyhow::Result<()> {
    let chat_id = msg.chat.id.0;
    let user_text = extract_text(msg).unwrap_or_default();

    let window = working::get_window(&deps.redis, chat_id, deps.config.memory.working_window_size)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "get_window failed; proceeding without context");
            Vec::new()
        });

    let mut messages = Vec::with_capacity(2 + window.len());
    messages.push(LlmMessage::system(SYSTEM_PROMPT_BASE));
    for w in &window {
        messages.push(LlmMessage::user(format_window_msg(w)));
    }
    let me = bot.get_me().await?;
    let me_username = me.username();
    let user_role = format_current_msg(msg, &user_text);
    messages.push(LlmMessage::user(user_role));

    let model = deps.config.openrouter.model_main.clone();
    let started = std::time::Instant::now();
    let completion = deps
        .openrouter
        .chat_completion(&model, &messages, REPLY_MAX_TOKENS)
        .await?;

    tracing::info!(
        chat_id,
        model = %completion.model,
        latency_ms = completion.latency_ms,
        prompt_tokens = completion.prompt_tokens,
        completion_tokens = completion.completion_tokens,
        total_tokens = completion.total_tokens,
        wall_ms = started.elapsed().as_millis() as u64,
        "llm reply"
    );

    let reply_text = completion.content.trim();
    if reply_text.is_empty() {
        tracing::warn!(chat_id, "llm returned empty content; skipping send");
        return Ok(());
    }

    let sent = bot
        .send_message(msg.chat.id, reply_text)
        .reply_parameters(ReplyParameters::new(MessageId(msg.id.0)))
        .await?;

    persist_bot_reply(&sent, me.id.0 as i64, me_username, deps).await?;
    Ok(())
}

async fn persist_bot_reply(
    sent: &Message,
    bot_id: i64,
    bot_username: &str,
    deps: &Deps,
) -> anyhow::Result<()> {
    let chat_id = sent.chat.id.0;
    let chat_title = chat_label(&sent.chat);
    let tg_message_id = sent.id.0 as i64;
    let text = clip_text(extract_text(sent));
    let username = Some(bot_username.to_string());
    let first_name = bot_username.to_string();

    sqlx::query!(
        r#"INSERT INTO users (id, username, first_name)
           VALUES (?, ?, ?)
           ON CONFLICT(id) DO UPDATE SET
             username = excluded.username,
             first_name = excluded.first_name"#,
        bot_id,
        username,
        first_name
    )
    .execute(&deps.sqlite)
    .await?;

    sqlx::query!(
        r#"INSERT INTO chats (id, title)
           VALUES (?, ?)
           ON CONFLICT(id) DO UPDATE SET title = excluded.title"#,
        chat_id,
        chat_title
    )
    .execute(&deps.sqlite)
    .await?;

    let has_media_zero: i64 = 0;
    sqlx::query!(
        r#"INSERT INTO messages (chat_id, user_id, tg_message_id, text, has_media)
           VALUES (?, ?, ?, ?, ?)"#,
        chat_id,
        bot_id,
        tg_message_id,
        text,
        has_media_zero
    )
    .execute(&deps.sqlite)
    .await?;

    if let Some(t) = text {
        let entry = WorkingMessage {
            user_id: bot_id,
            username: Some(bot_username.to_string()),
            text: t,
            media_desc: None,
            ts: sent.date.timestamp(),
        };
        working::push(
            &deps.redis,
            chat_id,
            &entry,
            deps.config.memory.working_window_size,
            deps.config.memory.working_ttl_days,
        )
        .await?;
    }
    Ok(())
}

fn format_window_msg(w: &WorkingMessage) -> String {
    let who = w
        .username
        .as_deref()
        .map(String::from)
        .unwrap_or_else(|| format!("uid:{}", w.user_id));
    if let Some(desc) = &w.media_desc {
        format!("{who}: {} [{}]", w.text, desc)
    } else {
        format!("{who}: {}", w.text)
    }
}

fn format_current_msg(msg: &Message, text: &str) -> String {
    let who = msg
        .from
        .as_ref()
        .and_then(|u| u.username.clone())
        .or_else(|| msg.from.as_ref().map(|u| u.first_name.clone()))
        .unwrap_or_else(|| "anon".into());
    format!("{who}: {text}")
}

fn chat_label(chat: &Chat) -> Option<String> {
    chat.title()
        .map(String::from)
        .or_else(|| chat.username().map(String::from))
}

fn extract_text(msg: &Message) -> Option<String> {
    msg.text().or_else(|| msg.caption()).map(String::from)
}

fn clip_text(t: Option<String>) -> Option<String> {
    t.map(|s| {
        if s.chars().count() > TEXT_MAX_LEN {
            s.chars().take(TEXT_MAX_LEN).collect()
        } else {
            s
        }
    })
}

fn detect_media(msg: &Message) -> bool {
    msg.photo().is_some()
        || msg.video().is_some()
        || msg.voice().is_some()
        || msg.video_note().is_some()
        || msg.document().is_some()
        || msg.audio().is_some()
        || msg.sticker().is_some()
        || msg.animation().is_some()
}
