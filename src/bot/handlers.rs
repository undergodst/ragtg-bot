use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{Chat, MessageId, ReplyParameters};

use crate::decision;
use crate::deps::Deps;
use crate::llm::client::Message as LlmMessage;
use crate::llm::perception;
use crate::llm::prompts::system::FULL_SYSTEM_PROMPT;
use crate::memory::episodic;
use crate::memory::events;
use crate::memory::semantic;
use crate::memory::working::{self, WorkingMessage};
use crate::metrics;
use crate::storage::redis as rl;
use crate::tasks::{extract_facts, summarize};

const TEXT_PREVIEW_LEN: usize = 100;
const TEXT_MAX_LEN: usize = 8000;
const REPLY_MAX_TOKENS: u32 = 2000;
const MEDIA_CACHE_TTL_DAYS: u32 = 30;
/// Upper bound on file size we'll send to the vision model. 10MB covers
/// every Telegram photo (which the platform itself caps at 10MB) and most
/// voice messages, while keeping base64-inflated request bodies under
/// reasonable LLM-provider limits.
const MAX_MEDIA_BYTES: usize = 10 * 1024 * 1024;

/// Persist every incoming message that has a `from` user, then maybe reply
/// (if the bot was mentioned or replied to). Errors are logged but not
/// propagated — losing a row of stats or a single LLM call isn't worth
/// crashing the dispatcher.
pub async fn handle_message(bot: Bot, msg: Message, deps: Deps) -> ResponseResult<()> {
    metrics::MESSAGES_RECEIVED.inc();

    // Persist with no description yet — async vision task will fill it in.
    if let Err(e) = save_message(&msg, &deps, None).await {
        tracing::warn!(error = %e, chat_id = msg.chat.id.0, "failed to persist message");
    }

    // Spawn detached perception task: download → describe → cache → UPDATE
    // SQLite + patch working-window. Failures only log; never block message
    // flow. Cache lookups are cheap, so we always go through perceive_media,
    // which short-circuits on hit.
    if detect_media(&msg) {
        let bot_clone = bot.clone();
        let msg_clone = msg.clone();
        let deps_clone = deps.clone();
        tokio::spawn(async move {
            if let Err(e) = perceive_and_persist(&bot_clone, &msg_clone, &deps_clone).await {
                tracing::warn!(
                    error = %e,
                    chat_id = msg_clone.chat.id.0,
                    tg_message_id = msg_clone.id.0,
                    "background perception failed"
                );
            }
        });
    }

    // Fire-and-forget background summarization check (cheap Redis INCR on
    // most calls; only does real work every N messages).
    {
        let deps_bg = deps.clone();
        let chat_id_bg = msg.chat.id.0;
        let user_id_bg = msg.from.as_ref().map(|u| u.id.0 as i64);
        tokio::spawn(async move {
            if let Err(e) = summarize::maybe_summarize(&deps_bg, chat_id_bg).await {
                tracing::warn!(error = %e, chat_id = chat_id_bg, "episodic summarize failed");
            }
            // Also trigger per-user fact extraction.
            if let Some(uid) = user_id_bg {
                if let Err(e) = extract_facts::maybe_extract_facts(&deps_bg, chat_id_bg, uid).await
                {
                    tracing::warn!(error = %e, chat_id = chat_id_bg, user_id = uid, "facts extraction failed");
                }
            }
        });
    }

    match decision::should_reply(&bot, &msg, &deps).await {
        Ok(true) => {
            tracing::info!(chat_id = msg.chat.id.0, "decision: replying to message");
            match rate_limit_pass(&msg, &deps).await {
                Ok(true) => {
                    if let Err(e) = reply(&bot, &msg, &deps).await {
                        tracing::error!(error = %e, "reply failed");
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
            tracing::debug!(chat_id, user_id, "user rate-limited");
            metrics::RATE_LIMITED.inc();
            return Ok(false);
        }
    }

    if !rl::check_chat_quota(&deps.redis, chat_id, cfg.chat_max_per_min).await? {
        tracing::debug!(chat_id, "chat rate-limited");
        metrics::RATE_LIMITED.inc();
        return Ok(false);
    }
    Ok(true)
}

async fn save_message(msg: &Message, deps: &Deps, media_desc: Option<&str>) -> anyhow::Result<()> {
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
    let media_desc_owned = media_desc.map(|s| s.to_string());

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
        r#"INSERT INTO messages (chat_id, user_id, tg_message_id, text, has_media, media_description)
           VALUES (?, ?, ?, ?, ?, ?)"#,
        chat_id,
        user_id,
        tg_message_id,
        text,
        has_media,
        media_desc_owned
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

    // Push to working memory if there is anything semantic to remember:
    // either a text body, or a media description (image/voice). A bare
    // `[photo]` with no description would dilute the prompt with noise.
    let working_text = text.clone().unwrap_or_default();
    let has_any_content = !working_text.is_empty()
        || media_desc_owned.is_some()
        || has_media == 1;
    if has_any_content {
        let entry = WorkingMessage {
            user_id,
            username: username.clone(),
            text: working_text,
            media_desc: media_desc_owned.clone(),
            ts: msg.date.timestamp(),
            tg_message_id: Some(tg_message_id),
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

async fn reply(bot: &Bot, msg: &Message, deps: &Deps) -> anyhow::Result<()> {
    let chat_id = msg.chat.id.0;
    let user_text = extract_text(msg).unwrap_or_default();

    let window = working::get_window(&deps.redis, chat_id, deps.config.memory.working_window_size)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "get_window failed; proceeding without context");
            Vec::new()
        });

    // On-demand perception: if the message or what it replies to has media, see it now.
    let mut current_media_desc = None;
    if detect_media(msg) {
        tracing::info!("detect_media found media in current message");
        current_media_desc = perceive_media(bot, msg, deps).await.unwrap_or(None);
    } else if let Some(reply_to) = msg.reply_to_message() {
        tracing::info!("checking reply_to_message for media");
        if detect_media(reply_to) {
            tracing::info!("detect_media found media in replied message");
            current_media_desc = perceive_media(bot, reply_to, deps).await.unwrap_or(None);
        } else {
            tracing::info!("no media found in replied message");
        }
    }

    if let Some(ref desc) = current_media_desc {
        tracing::info!(desc = %desc, "media perceived successfully");
    }

    // Compute query embedding once for all RAG retrievals.
    let query_vector = deps
        .embeddings
        .embed_single(&user_text)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to embed query; proceeding with zero vector");
            vec![0.0; crate::storage::qdrant::VECTOR_DIM as usize] // BGE-M3 dim — must match Qdrant collections, else search errors.
        });

    // Retrieve relevant episodic summaries (long-term memory).
    let episodic_summaries =
        episodic::retrieve_relevant_summaries(deps, chat_id, &query_vector).await;
    tracing::info!(
        count = episodic_summaries.len(),
        "RAG: retrieved episodic summaries"
    );

    // Retrieve facts about users in the working window.
    let user_facts =
        semantic::retrieve_facts_for_window_users(deps, chat_id, &window, &query_vector).await;
    tracing::info!(count = user_facts.len(), "RAG: retrieved user facts");

    // Inject relevant chat events (auto-curated memory of significant moments).
    let chat_events = events::retrieve_relevant(deps, chat_id, &query_vector).await;
    tracing::info!(count = chat_events.len(), "RAG: retrieved chat events");

    let mut messages = Vec::with_capacity(4 + episodic_summaries.len() + window.len());
    messages.push(LlmMessage::system(&*FULL_SYSTEM_PROMPT));

    // Inject episodic context between system prompt and working window.
    if !episodic_summaries.is_empty() {
        let mut ctx = String::from("[Релевантный контекст из прошлого чата]:\n");
        for s in &episodic_summaries {
            ctx.push_str(&format!("- {s}\n"));
        }
        messages.push(LlmMessage::system(ctx));
    }

    // Inject known facts about users.
    if !user_facts.is_empty() {
        let mut ctx = String::from("[Известные факты о участниках]:\n");
        for (username, facts) in &user_facts {
            ctx.push_str(&format!("@{username}:\n"));
            for f in facts {
                ctx.push_str(&format!("  - {f}\n"));
            }
        }
        messages.push(LlmMessage::system(ctx));
    }

    if !chat_events.is_empty() {
        let mut ctx = String::from("[Релевантные моменты из этого чата]:\n");
        for e in &chat_events {
            ctx.push_str(&format!("- {e}\n"));
        }
        messages.push(LlmMessage::system(ctx));
    }

    for w in &window {
        messages.push(LlmMessage::user(format_window_msg(w)));
    }
    let me_username = &deps.bot_username;
    let user_role = format_current_msg(msg, &user_text);
    let user_role = if let Some(desc) = current_media_desc {
        format!("{} [Изображение/Голосовое: {}]", user_role, desc)
    } else {
        user_role
    };
    messages.push(LlmMessage::user(user_role));

    let model = deps.config.openrouter.model_main.clone();
    let started = std::time::Instant::now();
    tracing::info!(chat_id, model = %model, msg_count = messages.len(), "calling main LLM for reply");
    tracing::debug!(chat_id, messages = ?messages, "main LLM request payload");

    let completion = deps
        .openrouter
        .chat_completion("reply", &model, &messages, REPLY_MAX_TOKENS)
        .await?;

    let reply_text = completion.content.trim();

    tracing::info!(
        chat_id,
        model = %completion.model,
        latency_ms = completion.latency_ms,
        prompt_tokens = completion.prompt_tokens,
        completion_tokens = completion.completion_tokens,
        total_tokens = completion.total_tokens,
        wall_ms = started.elapsed().as_millis() as u64,
        reply = %reply_text,
        "llm reply"
    );

    if reply_text.is_empty() {
        tracing::warn!(chat_id, "llm returned empty content; skipping send");
        return Ok(());
    }

    let sent = bot
        .send_message(msg.chat.id, reply_text)
        .reply_parameters(ReplyParameters::new(MessageId(msg.id.0)))
        .await?;

    persist_bot_reply(&sent, deps.bot_id, me_username, deps).await?;
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
            tg_message_id: Some(tg_message_id),
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

pub(crate) fn detect_media(msg: &Message) -> bool {
    msg.photo().is_some()
        || msg.video().is_some()
        || msg.voice().is_some()
        || msg.video_note().is_some()
        || msg.document().is_some()
        || msg.audio().is_some()
        || msg.sticker().is_some()
        || msg.animation().is_some()
}

/// What we can usefully send to the perception sub-agent. Other media
/// kinds (video, video_note/circle, animated stickers, generic documents)
/// are intentionally absent for the v1 vision pipeline — see CLAUDE.md
/// step 6 scope.
enum Perceived {
    Image { file_id: String, mime: String },
    Voice { file_id: String },
}

fn classify_media(msg: &Message) -> Option<Perceived> {
    if let Some(photos) = msg.photo()
        && let Some(largest) = photos.last()
    {
        return Some(Perceived::Image {
            file_id: largest.file.id.clone(),
            mime: "image/jpeg".into(),
        });
    }
    if let Some(voice) = msg.voice() {
        return Some(Perceived::Voice {
            file_id: voice.file.id.clone(),
        });
    }
    if let Some(sticker) = msg.sticker() {
        // Static webp stickers are just images. Animated (TGS Lottie) and
        // video stickers (webm) need extra decoding we don't do yet.
        if !sticker.is_animated() && !sticker.is_video() {
            return Some(Perceived::Image {
                file_id: sticker.file.id.clone(),
                mime: "image/webp".into(),
            });
        }
    }
    if let Some(doc) = msg.document()
        && let Some(mime) = doc.mime_type.as_ref()
        && mime.essence_str().starts_with("image/")
    {
        return Some(Perceived::Image {
            file_id: doc.file.id.clone(),
            mime: mime.essence_str().to_string(),
        });
    }
    None
}

/// Download → SHA256 → cache lookup → (on miss) acquire vision slot →
/// describe → cache write → release. Errors are mapped to `Ok(None)` at
/// the top level so a flaky perception call never blocks message saving.
async fn perceive_media(bot: &Bot, msg: &Message, deps: &Deps) -> anyhow::Result<Option<String>> {
    let Some(kind) = classify_media(msg) else {
        return Ok(None);
    };
    let chat_id = msg.chat.id.0;

    let file_id = match &kind {
        Perceived::Image { file_id, .. } | Perceived::Voice { file_id } => file_id.clone(),
    };
    let bytes = match download_file_bytes(bot, &file_id).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "media download failed");
            return Ok(None);
        }
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > MAX_MEDIA_BYTES {
        tracing::info!(
            bytes = bytes.len(),
            "media exceeds size cap; skipping perception"
        );
        return Ok(None);
    }

    let sha = sha256_hex(&bytes);

    tracing::info!(chat_id, sha = %sha, "Perception: checking media cache");
    if let Ok(Some(cached)) = rl::get_media_desc(&deps.redis, &sha).await {
        tracing::info!(chat_id, sha = %sha, "Perception: cache hit, reusing description");
        metrics::VISION_CACHE_HIT_TOTAL.inc();
        return Ok(Some(cached));
    }

    tracing::info!(chat_id, "Perception: cache miss, calling vision model...");

    let max_slots = deps.config.ratelimit.vision_concurrent;
    if !rl::acquire_vision_slot(&deps.redis, max_slots).await? {
        tracing::info!(
            chat_id = msg.chat.id.0,
            "vision slots full; skipping perception"
        );
        return Ok(None);
    }

    // Run the LLM call inside an async block so the slot release runs on
    // both Ok and Err paths without an explicit defer wrapper. Releasing
    // before evaluating `result` would let the slot return to the pool
    // before the cache write — fine, since the cache write is cheap.
    let result = match &kind {
        Perceived::Image { mime, .. } => {
            perception::describe_image(
                &deps.openrouter,
                &bytes,
                mime,
                &deps.config.openrouter.model_vision,
                &deps.config.openrouter.vision_fallbacks,
            )
            .await
        }
        Perceived::Voice { .. } => {
            perception::transcribe_voice(
                &deps.openrouter,
                &bytes,
                &deps.config.openrouter.model_vision,
            )
            .await
        }
    };
    if let Err(e) = rl::release_vision_slot(&deps.redis).await {
        tracing::warn!(error = %e, "vision slot release failed");
    }

    match result {
        Ok(desc) => {
            if let Err(e) = rl::put_media_desc(&deps.redis, &sha, &desc, MEDIA_CACHE_TTL_DAYS).await
            {
                tracing::warn!(error = %e, "media cache write failed");
            }
            Ok(Some(desc))
        }
        Err(e) => {
            tracing::warn!(error = %e, "perception call failed");
            Ok(None)
        }
    }
}

/// Background perception pipeline: describe media, cache, write description
/// into both SQLite (`messages.media_description`) and the chat's Redis
/// working window. Errors propagate up and get logged at the spawn site.
async fn perceive_and_persist(bot: &Bot, msg: &Message, deps: &Deps) -> anyhow::Result<()> {
    let Some(desc) = perceive_media(bot, msg, deps).await? else {
        return Ok(()); // not classified, too big, slot full, or all models failed
    };

    let chat_id = msg.chat.id.0;
    let tg_message_id = msg.id.0 as i64;

    sqlx::query!(
        r#"UPDATE messages
           SET media_description = ?
           WHERE chat_id = ? AND tg_message_id = ?"#,
        desc,
        chat_id,
        tg_message_id,
    )
    .execute(&deps.sqlite)
    .await?;

    let patched = working::patch_media_desc(&deps.redis, chat_id, tg_message_id, &desc).await?;

    tracing::info!(
        chat_id,
        tg_message_id,
        patched_window = patched,
        desc_len = desc.chars().count(),
        "perception persisted"
    );
    Ok(())
}

async fn download_file_bytes(bot: &Bot, file_id: &str) -> anyhow::Result<Vec<u8>> {
    let file = bot.get_file(file_id.to_string()).await?;
    let mut stream = bot.download_file_stream(&file.path);
    let mut buf: Vec<u8> = Vec::with_capacity(file.size as usize);
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        if buf.len() + bytes.len() > MAX_MEDIA_BYTES {
            anyhow::bail!("media exceeds {MAX_MEDIA_BYTES} bytes during stream");
        }
        buf.extend_from_slice(&bytes);
    }
    Ok(buf)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}
