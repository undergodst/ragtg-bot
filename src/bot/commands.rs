use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::deps::Deps;
use crate::memory::working;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "snake_case", description = "Команды:")]
pub enum Command {
    #[command(description = "приветствие")]
    Start,
    #[command(description = "показать список команд")]
    Help,
    #[command(description = "информация о боте и моделях")]
    Info,
    #[command(description = "проверка живости")]
    Ping,
    #[command(description = "сколько сообщений в этом чате")]
    Stats,
    #[command(description = "[admin] показать working memory чата")]
    Window,
    #[command(description = "задать вопрос ИИ-ассистенту (1 раз в 1 минуту)")]
    Ask(String),
    #[command(description = "задать вопрос безлимитной бесплатной ИИ-модели", rename = "askfree")]
    AskFree(String),
}

pub async fn handle(bot: Bot, msg: Message, cmd: Command, deps: Deps) -> ResponseResult<()> {
    match cmd {
        Command::Start => {
            let aliases = deps
                .config
                .bot
                .aliases
                .iter()
                .map(|a| format!("<b>{}</b>", html_escape(a)))
                .collect::<Vec<_>>()
                .join(", ");
            let aliases_line = if aliases.is_empty() {
                String::new()
            } else {
                format!("\n<b>Чтобы позвать меня</b>, напиши: {aliases} или сделай реплай на любое моё сообщение.")
            };
            let start_text = format!(
                "👋 <b>Здарова! Я — твой новый ИИ-сожитель.</b>\n\
\n\
Я не просто бот формата «вопрос-ответ». Я полноценный участник чата:\n\
• 🧠 <b>Помню историю</b> — слежу за контекстом и учитываю прошлые сообщения.\n\
• 🖼️ <b>Вижу медиа</b> — кидай фото, голосовые, стикеры — разберу.\n\
• 📚 <b>Запоминаю моменты</b> — значимые цитаты и события чата сами оседают в памяти.\n\
• 🎭 <b>Имею характер</b> — могу потроллить, могу и дельно посоветовать.\n\
• 🤖 <b>Сам решаю</b>, когда встрять в разговор: на упоминание или реплай.\n\
\n\
<b>📡 Команды:</b>\n\
/help — список всех команд\n\
/info — модели и потроха\n\
/ping — жив ли я\n\
/stats — сколько сообщений в этом чате\n\
/ask &lt;вопрос&gt; — спросить умную PRO-модель (кулдаун 1 минута)\n\
/askfree &lt;вопрос&gt; — спросить мощную бесплатную модель (без лимитов)\n\
\n\
<b>🛠 Для админов:</b>\n\
/window — короткая память бота\n\
{aliases_line}\n\
\n\
Погнали наводить суету.",
            );
            bot.send_message(msg.chat.id, start_text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        }
        Command::Help => {
            let help_text = "\
<b>Я — местный ИИ-житель этого чата.</b>\n\
Читаю сообщения, смотрю картинки и сам решаю, когда стоит встрять.\n\
\n\
<b>Базовые:</b>\n\
/start — приветствие и полная инфа\n\
/help — эта справка\n\
/info — модели и архитектура\n\
/ping — проверить, жив ли я\n\
/stats — сколько сообщений в чате\n\
\n\
<b>Вопросы LLM:</b>\n\
/ask &lt;вопрос&gt; — умная PRO-модель (кулдаун 1 минута)\n\
/askfree &lt;вопрос&gt; — мощная бесплатная модель (безлимит)\n\
\n\
<b>Память:</b> сам запоминаю значимые моменты чата (цитаты, мемы, события).\n\
\n\
<b>Для админов:</b>\n\
/window — посмотреть мою короткую память (последние сообщения)\n\
\n\
<b>Чтобы позвать меня:</b> напиши «пидрила», «бот» или «антиграв», или сделай реплай на любое моё сообщение.";
            bot.send_message(msg.chat.id, help_text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        }
        Command::Info => {
            let or = &deps.config.openrouter;
            let info_text = format!(
                "<b>🤖 Что я умею:</b>\n\
• Слежу за контекстом и сам решаю, когда отвечать.\n\
• Понимаю картинки и голосовые.\n\
• Помню факты о пользователях и значимые моменты чата.\n\
\n\
<b>🧠 Модели (OpenRouter):</b>\n\
1. Общение в чате: <code>{main}</code>\n\
2. /ask (PRO): <code>{pro}</code>\n\
3. /askfree (FREE): <code>{ask_free}</code>\n\
4. Зрение/аудио: <code>{vision}</code>\n\
5. Решение «отвечать или нет»: <code>{decision}</code>\n\
\n\
<b>💾 Память:</b> SQLite + Qdrant (BGE-M3 эмбеддинги, 1024 dim) + Redis-кэш.",
                main = html_escape(&or.model_main),
                pro = html_escape(&or.model_pro),
                ask_free = html_escape(&or.model_ask_free),
                vision = html_escape(&or.model_vision),
                decision = html_escape(&or.model_decision),
            );
            bot.send_message(msg.chat.id, info_text)
                .parse_mode(teloxide::types::ParseMode::Html)
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
        Command::Ask(question) => handle_ask(&bot, &msg, &deps, &question).await?,
        Command::AskFree(question) => handle_ask_free(&bot, &msg, &deps, &question).await?,
    }
    Ok(())
}

fn is_admin(msg: &Message, deps: &Deps) -> bool {
    msg.from
        .as_ref()
        .map(|u| u.id.0 as i64)
        .is_some_and(|id| deps.config.bot.admin_ids.contains(&id))
}

// ── Ask command ────────────────────────────────────────────────────

async fn handle_ask(bot: &Bot, msg: &Message, deps: &Deps, question: &str) -> ResponseResult<()> {
    if question.trim().is_empty() {
        bot.send_message(msg.chat.id, "ты забыл написать вопрос после команды.")
            .await?;
        return Ok(());
    }

    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);

    if !is_admin(msg, deps) {
        let cooldown = crate::storage::redis::check_ask_cooldown(&deps.redis, user_id, 60)
            .await
            .unwrap_or(0);

        if cooldown > 0 {
            let minutes = cooldown / 60;
            let seconds = cooldown % 60;
            bot.send_message(
                msg.chat.id,
                format!("подожди, ты уже спрашивал. следующий вопрос можно будет задать через {minutes} мин. {seconds} сек."),
            )
            .reply_parameters(teloxide::types::ReplyParameters::new(teloxide::types::MessageId(msg.id.0)))
            .await?;
            return Ok(());
        }
    }

    let messages = vec![
        crate::llm::client::Message::system(
            "Ты — Пидрила, циничный ИИ-тролль с имиджборд. \
            Сразу отвечай по сути вопроса. Без предисловий, без «давайте подумаем», без мета-рассуждений. \
            Ответ должен быть точным, глубоким и в твоём стиле.\n\
            \n\
            ОФОРМЛЕНИЕ — ПОСЛЕДНИЙ ШАГ. Сначала готовый ответ, и только потом, если уместно, оборачиваешь \
            ключевые места в HTML: <b>жирный</b>, <i>курсив</i>, <code>код</code>, <pre>блоки кода/таблицы</pre>, \
            <blockquote>цитата</blockquote>, <tg-spoiler>спойлер</tg-spoiler>. Все теги закрывай. \
            Если разметка не нужна — не вставляй её ради разметки.",
        ),
        crate::llm::client::Message::user(question),
    ];

    let model = deps.config.openrouter.model_pro.clone();
    stream_to_chat(bot, msg, deps, &model, messages, "/ask").await
}

async fn handle_ask_free(bot: &Bot, msg: &Message, deps: &Deps, question: &str) -> ResponseResult<()> {
    if question.trim().is_empty() {
        bot.send_message(msg.chat.id, "ты забыл написать вопрос после команды.")
            .await?;
        return Ok(());
    }

    let messages = vec![
        crate::llm::client::Message::system(
            "Ты — Пидрила в режиме «Максимальный анализ». Дай развёрнутый, структурированный ответ в стиле 2ch-философа: душно, но дико умно. \
            Сразу к делу — без «давайте подумаем», без анализа постановки вопроса, без вступлений типа «начнём с того что...». \
            Первое предложение — уже часть ответа.\n\
            \n\
            ОФОРМЛЕНИЕ — ПОСЛЕДНИЙ ШАГ. Сначала готовый ответ, и только потом размечаешь HTML: \
            <b>жирный</b>, <i>курсив</i>, <code>код</code>, <pre>блоки/таблицы</pre>, <blockquote>цитата</blockquote>, <tg-spoiler>спойлер</tg-spoiler>. \
            Все теги закрывай.",
        ),
        crate::llm::client::Message::user(question),
    ];

    let model = deps.config.openrouter.model_ask_free.clone();
    stream_to_chat(bot, msg, deps, &model, messages, "/askfree").await
}

/// Shared streaming pipeline for /ask and /askfree.
///
/// Handles: stream-start retry, intermediate "writing..." edits, fallback to
/// `reasoning` field when `content` is empty (some thinking-models route the
/// answer there), chunking responses past Telegram's 4096-char limit, and
/// HTML parse mode with plain-text fallback on parse errors.
async fn stream_to_chat(
    bot: &Bot,
    msg: &Message,
    deps: &Deps,
    model: &str,
    messages: Vec<crate::llm::client::Message>,
    label: &str,
) -> ResponseResult<()> {
    let chat_id = msg.chat.id;
    let chat_id_num = chat_id.0;

    let wait_msg = bot
        .send_message(chat_id, "⏳ Генерирую ответ...")
        .reply_parameters(teloxide::types::ReplyParameters::new(teloxide::types::MessageId(
            msg.id.0,
        )))
        .await?;

    tracing::info!(chat_id = chat_id_num, model = %model, label, "calling model (streaming)");
    tracing::debug!(chat_id = chat_id_num, label, messages = ?messages, "stream request payload");

    // Retry stream-start on transient errors (the underlying client only
    // retries non-streaming calls; without this, a single 429 burns the
    // whole user request).
    let mut attempt: u32 = 0;
    let max_attempts: u32 = 3;
    let mut stream = loop {
        // disable_thinking=true: tell OpenRouter to suppress chain-of-thought
        // for thinking-models (Kimi, R1, etc.) so we don't waste latency on
        // reasoning we'd just discard — final answer comes via `content`.
        match deps.openrouter.chat_completion_stream(model, &messages, 30000, true).await {
            Ok(s) => break s,
            Err(e) if attempt + 1 < max_attempts => {
                let delay_ms = 500u64 << attempt.min(3);
                tracing::warn!(error = %e, attempt, delay_ms, label, "stream start failed; retrying");
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                attempt += 1;
            }
            Err(e) => {
                tracing::error!(error = %e, label, "failed to start stream after retries");
                bot.edit_message_text(chat_id, wait_msg.id, "Сервера модели перегружены или недоступны. Попробуй немного позже.").await?;
                return Ok(());
            }
        }
    };

    use futures_util::StreamExt;
    let started = std::time::Instant::now();
    let mut full_content = String::new();
    let mut last_update = std::time::Instant::now();
    let update_interval = std::time::Duration::from_millis(1500);

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, label, "stream chunk error; ending early");
                break;
            }
        };

        if let Some(choice) = chunk.choices.first() {
            // We deliberately ignore `delta.reasoning` here — `disable_thinking`
            // suppresses CoT at the OpenRouter side, and even if a stray
            // reasoning chunk slips through we don't want it surfacing in
            // the chat (per user request: model should not "think out loud"
            // in the TG message).
            if let Some(ref c) = choice.delta.content {
                full_content.push_str(c);
            }

            if last_update.elapsed() > update_interval {
                let status = if full_content.is_empty() {
                    "⏳ Думаю...".to_string()
                } else {
                    let preview = take_last_chars(&full_content, 3800);
                    format!("✍️ Пишу ответ...\n\n{preview}")
                };
                let _ = bot.edit_message_text(chat_id, wait_msg.id, status).await;
                last_update = std::time::Instant::now();
            }
        }
    }

    if full_content.trim().is_empty() {
        let _ = bot.edit_message_text(chat_id, wait_msg.id, "Модель выдала пустой ответ. Попробуй еще раз.").await;
        return Ok(());
    }
    let final_text = full_content;

    tracing::info!(
        chat_id = chat_id_num,
        label,
        latency_ms = started.elapsed().as_millis() as u64,
        len = final_text.chars().count(),
        "stream completed"
    );

    // Telegram caps message text at 4096 chars; chunk to be safe (use 3900
    // to leave headroom for entity markup expansion). First chunk replaces
    // the wait message via edit; remaining chunks are sent as new messages.
    let chunks = chunk_message(&final_text, 3900);
    let mut iter = chunks.into_iter();
    if let Some(first) = iter.next() {
        let edit_html = bot
            .edit_message_text(chat_id, wait_msg.id, first.clone())
            .parse_mode(teloxide::types::ParseMode::Html)
            .await;
        if let Err(e) = edit_html {
            tracing::warn!(error = %e, label, "HTML edit failed; sending first chunk as plain text");
            if let Err(e2) = bot.edit_message_text(chat_id, wait_msg.id, first).await {
                tracing::error!(error = %e2, label, "plain-text edit also failed");
            }
        }
    }
    for rest in iter {
        let send_html = bot
            .send_message(chat_id, rest.clone())
            .parse_mode(teloxide::types::ParseMode::Html)
            .await;
        if let Err(e) = send_html {
            tracing::warn!(error = %e, label, "HTML send failed; sending chunk as plain text");
            if let Err(e2) = bot.send_message(chat_id, rest).await {
                tracing::error!(error = %e2, label, "plain-text send also failed");
            }
        }
    }

    Ok(())
}

fn take_last_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        s.to_string()
    } else {
        s.chars().skip(count - n).collect()
    }
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

/// Escape text for safe injection into Telegram HTML parse mode.
/// Only `&`, `<`, `>` need escaping inside text nodes.
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
