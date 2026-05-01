use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::deps::Deps;
use crate::memory::{lore, working};

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "snake_case", description = "Команды:")]
pub enum Command {
    #[command(description = "приветствие")]
    Start,
    #[command(description = "проверка живости")]
    Ping,
    #[command(description = "сколько сообщений в этом чате")]
    Stats,
    #[command(description = "[admin] показать working memory чата")]
    Window,
    #[command(description = "[admin] добавить лор: /lore_add текст")]
    LoreAdd(String),
    #[command(description = "[admin] список лора")]
    LoreList,
    #[command(description = "[admin] удалить лор: /lore_del ID")]
    LoreDel(String),
}

pub async fn handle(bot: Bot, msg: Message, cmd: Command, deps: Deps) -> ResponseResult<()> {
    match cmd {
        Command::Start => {
            bot.send_message(msg.chat.id, "ну привет, чем тут у нас занимаемся.")
                .await?;
        }
        Command::Ping => {
            bot.send_message(msg.chat.id, "pong").await?;
        }
        Command::Stats => {
            let chat_id = msg.chat.id.0;
            match count_messages(&deps, chat_id).await {
                Ok(count) => {
                    bot.send_message(msg.chat.id, format!("в этом чате {count} сообщений"))
                        .await?;
                }
                Err(e) => {
                    tracing::warn!(error = %e, chat_id, "stats query failed");
                    bot.send_message(msg.chat.id, "не смог посчитать, ну и хрен с ним")
                        .await?;
                }
            }
        }
        Command::Window => handle_window(&bot, &msg, &deps).await?,
        Command::LoreAdd(text) => handle_lore_add(&bot, &msg, &deps, &text).await?,
        Command::LoreList => handle_lore_list(&bot, &msg, &deps).await?,
        Command::LoreDel(id_str) => handle_lore_del(&bot, &msg, &deps, &id_str).await?,
    }
    Ok(())
}

fn is_admin(msg: &Message, deps: &Deps) -> bool {
    msg.from
        .as_ref()
        .map(|u| u.id.0 as i64)
        .is_some_and(|id| deps.config.bot.admin_ids.contains(&id))
}

// ── Lore commands ──────────────────────────────────────────────────

async fn handle_lore_add(bot: &Bot, msg: &Message, deps: &Deps, text: &str) -> ResponseResult<()> {
    if !is_admin(msg, deps) {
        return Ok(());
    }

    let text = text.trim();
    if text.is_empty() {
        bot.send_message(msg.chat.id, "использование: /lore_add <текст лора>")
            .await?;
        return Ok(());
    }

    let chat_id = msg.chat.id.0;
    let added_by = msg.from.as_ref().map(|u| u.id.0 as i64);

    match lore::add_lore(deps, chat_id, text, added_by, None).await {
        Ok(id) => {
            bot.send_message(msg.chat.id, format!("✅ лор #{id} добавлен"))
                .await?;
        }
        Err(e) => {
            tracing::warn!(error = %e, chat_id, "lore_add failed");
            bot.send_message(msg.chat.id, "не получилось добавить лор 😔")
                .await?;
        }
    }
    Ok(())
}

async fn handle_lore_list(bot: &Bot, msg: &Message, deps: &Deps) -> ResponseResult<()> {
    if !is_admin(msg, deps) {
        return Ok(());
    }

    let chat_id = msg.chat.id.0;
    match lore::list_lore(deps, chat_id).await {
        Ok(entries) => {
            if entries.is_empty() {
                bot.send_message(msg.chat.id, "лор пуст").await?;
            } else {
                let mut body = format!("📜 Лор ({} записей):\n\n", entries.len());
                for (id, text) in &entries {
                    let preview: String = text.chars().take(100).collect();
                    body.push_str(&format!("#{id}: {preview}\n"));
                }
                for chunk in chunk_message(&body, 4000) {
                    bot.send_message(msg.chat.id, chunk).await?;
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, chat_id, "lore_list failed");
            bot.send_message(msg.chat.id, "не смог загрузить лор").await?;
        }
    }
    Ok(())
}

async fn handle_lore_del(bot: &Bot, msg: &Message, deps: &Deps, id_str: &str) -> ResponseResult<()> {
    if !is_admin(msg, deps) {
        return Ok(());
    }

    let id_str = id_str.trim();
    let lore_id: i64 = match id_str.parse() {
        Ok(id) => id,
        Err(_) => {
            bot.send_message(msg.chat.id, "использование: /lore_del <ID>")
                .await?;
            return Ok(());
        }
    };

    match lore::delete_lore(deps, lore_id).await {
        Ok(true) => {
            bot.send_message(msg.chat.id, format!("🗑 лор #{lore_id} удалён"))
                .await?;
        }
        Ok(false) => {
            bot.send_message(msg.chat.id, format!("лор #{lore_id} не найден"))
                .await?;
        }
        Err(e) => {
            tracing::warn!(error = %e, "lore_del failed");
            bot.send_message(msg.chat.id, "не получилось удалить").await?;
        }
    }
    Ok(())
}

// ── Window command ─────────────────────────────────────────────────

async fn handle_window(bot: &Bot, msg: &Message, deps: &Deps) -> ResponseResult<()> {
    if !is_admin(msg, deps) {
        return Ok(());
    }

    let chat_id = msg.chat.id.0;
    let window_size = deps.config.memory.working_window_size;
    let entries = match working::get_window(&deps.redis, chat_id, window_size).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, chat_id, "get_window failed");
            bot.send_message(msg.chat.id, format!("redis сломался: {e}"))
                .await?;
            return Ok(());
        }
    };

    let body = if entries.is_empty() {
        "окно пустое".to_string()
    } else {
        let mut out = format!("working window ({} msgs):\n", entries.len());
        for (i, e) in entries.iter().enumerate() {
            let who = e
                .username
                .as_deref()
                .map(|u| format!("@{u}"))
                .unwrap_or_else(|| format!("uid:{}", e.user_id));
            let text: String = e.text.chars().take(120).collect();
            let media = if e.media_desc.is_some() { " [media]" } else { "" };
            out.push_str(&format!("{}. {who}{media}: {text}\n", i + 1));
        }
        out
    };

    for chunk in chunk_message(&body, 4000) {
        bot.send_message(msg.chat.id, chunk).await?;
    }
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────

fn chunk_message(s: &str, limit: usize) -> Vec<String> {
    if s.chars().count() <= limit {
        return vec![s.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    for line in s.split_inclusive('\n') {
        // If a single line is itself longer than `limit`, hard-break it.
        if line.chars().count() > limit {
            if !buf.is_empty() {
                out.push(std::mem::take(&mut buf));
            }
            let mut piece = String::new();
            for c in line.chars() {
                piece.push(c);
                if piece.chars().count() >= limit {
                    out.push(std::mem::take(&mut piece));
                }
            }
            if !piece.is_empty() {
                buf = piece;
            }
            continue;
        }
        if buf.chars().count() + line.chars().count() > limit {
            out.push(std::mem::take(&mut buf));
        }
        buf.push_str(line);
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

async fn count_messages(deps: &Deps, chat_id: i64) -> anyhow::Result<i64> {
    let count = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM messages WHERE chat_id = ?"#,
        chat_id
    )
    .fetch_one(&deps.sqlite)
    .await?;
    Ok(count)
}
