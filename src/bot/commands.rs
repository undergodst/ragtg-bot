use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::deps::Deps;
use crate::memory::working;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Команды:")]
pub enum Command {
    #[command(description = "приветствие")]
    Start,
    #[command(description = "проверка живости")]
    Ping,
    #[command(description = "сколько сообщений в этом чате")]
    Stats,
    #[command(description = "[admin] показать working memory чата")]
    Window,
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
    }
    Ok(())
}

async fn handle_window(bot: &Bot, msg: &Message, deps: &Deps) -> ResponseResult<()> {
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64);
    if !user_id.is_some_and(|id| deps.config.bot.admin_ids.contains(&id)) {
        // Silently ignore for non-admins to avoid leaking the command's existence.
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

    // Telegram sendMessage caps at 4096 chars; with 30×120-char entries the
    // body can hit ~5kB, so chunk into 4000-char pieces (leaves headroom for
    // multi-byte chars expanding under MarkdownV2 escaping later).
    for chunk in chunk_message(&body, 4000) {
        bot.send_message(msg.chat.id, chunk).await?;
    }
    Ok(())
}

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
