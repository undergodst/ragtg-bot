use teloxide::prelude::*;
use teloxide::types::Chat;

use crate::deps::Deps;
use crate::memory::working::{self, WorkingMessage};

const TEXT_PREVIEW_LEN: usize = 100;
const TEXT_MAX_LEN: usize = 8000;

/// Persist every incoming message that has a `from` user. Errors are logged
/// but never propagated — losing a row of stats is not worth crashing on.
pub async fn handle_message(_bot: Bot, msg: Message, deps: Deps) -> ResponseResult<()> {
    if let Err(e) = save_message(&msg, &deps).await {
        tracing::warn!(error = %e, chat_id = msg.chat.id.0, "failed to persist message");
    }
    Ok(())
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

    // Working memory: only push messages that have visible text (or media
    // description, when we wire up vision). Empty entries pollute the prompt.
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
