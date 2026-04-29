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

    bot.send_message(msg.chat.id, body).await?;
    Ok(())
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
