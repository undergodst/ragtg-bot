use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::deps::Deps;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Команды:")]
pub enum Command {
    #[command(description = "приветствие")]
    Start,
    #[command(description = "проверка живости")]
    Ping,
    #[command(description = "сколько сообщений в этом чате")]
    Stats,
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
    }
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
