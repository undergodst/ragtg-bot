//! Decision layer: should the bot reply to this message?
//!
//! Один источник истины — три триггера, все с P=1.0:
//! 1. @mention бота
//! 2. Реплай на сообщение бота
//! 3. Алиас или username бота в тексте
//!
//! Иначе — бот молчит. Никакого рандома и LLM-классификатора.

use teloxide::prelude::*;
use teloxide::types::MessageEntityKind;

use crate::config::DecisionConfig;
use crate::deps::Deps;
use crate::metrics;

/// Решение: отвечать ли. Возвращает true для трёх триггеров и личных чатов.
pub async fn should_reply(_bot: &Bot, msg: &Message, deps: &Deps) -> anyhow::Result<bool> {
    // Сообщение должно содержать текст/подпись или медиа.
    let text = msg.text().or_else(|| msg.caption());
    let has_media = crate::bot::handlers::detect_media(msg);
    if text.is_none() && !has_media {
        return Ok(false);
    }

    // Личные чаты — всегда отвечаем.
    if msg.chat.is_private() {
        metrics::DECISION_OUTCOMES.with_label_values(&["reply"]).inc();
        return Ok(true);
    }

    let cfg = &deps.config.decision;
    let bot_username = &deps.bot_username;

    // Триггер 1: @mention бота.
    if has_mention(msg, bot_username) {
        metrics::DECISION_OUTCOMES.with_label_values(&["reply"]).inc();
        return Ok(cfg.mention_p >= 1.0);
    }

    // Триггер 2: реплай на сообщение бота.
    if let Some(reply_to) = msg.reply_to_message()
        && let Some(replied_user) = reply_to.from.as_ref()
        && replied_user.id.0 as i64 == deps.bot_id
    {
        metrics::DECISION_OUTCOMES.with_label_values(&["reply"]).inc();
        return Ok(cfg.reply_p >= 1.0);
    }

    // Триггер 3: алиас или username в тексте.
    if let Some(t) = text {
        let p = text_alias_p(t, &deps.config.bot.aliases, bot_username, cfg);
        if p >= 1.0 {
            metrics::DECISION_OUTCOMES.with_label_values(&["reply"]).inc();
            return Ok(true);
        }
    }

    metrics::DECISION_OUTCOMES.with_label_values(&["skip_no_trigger"]).inc();
    Ok(false)
}

/// Чистая функция для тестов: возвращает `cfg.alias_in_text_p` если в тексте
/// найден username бота или один из алиасов. Сравнение case-insensitive.
fn text_alias_p(text: &str, aliases: &[String], bot_username: &str, cfg: &DecisionConfig) -> f32 {
    let lower = text.to_lowercase();
    if lower.contains(&bot_username.to_lowercase()) {
        return cfg.alias_in_text_p;
    }
    for alias in aliases {
        if lower.contains(&alias.to_lowercase()) {
            return cfg.alias_in_text_p;
        }
    }
    0.0
}

/// @mention сразу через Telegram entities — надёжнее, чем substring.
fn has_mention(msg: &Message, bot_username: &str) -> bool {
    let entities = msg.parse_entities().or_else(|| msg.parse_caption_entities());
    let Some(entities) = entities else { return false };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DecisionConfig {
        DecisionConfig {
            mention_p: 1.0,
            reply_p: 1.0,
            alias_in_text_p: 1.0,
        }
    }

    #[test]
    fn alias_in_text_returns_one() {
        let aliases = vec!["пидрила".to_string(), "бот".to_string()];
        assert_eq!(
            text_alias_p("эй пидрила, как дела", &aliases, "nugpuJIa", &cfg()),
            1.0
        );
        assert_eq!(
            text_alias_p("ну бот ты даёшь", &aliases, "nugpuJIa", &cfg()),
            1.0
        );
    }

    #[test]
    fn bot_username_in_text_returns_one() {
        let aliases: Vec<String> = vec![];
        assert_eq!(
            text_alias_p("hey nugpuJIa what's up", &aliases, "nugpuJIa", &cfg()),
            1.0
        );
    }

    #[test]
    fn no_trigger_returns_zero() {
        let aliases = vec!["пидрила".to_string()];
        assert_eq!(
            text_alias_p("привет всем как дела", &aliases, "nugpuJIa", &cfg()),
            0.0
        );
    }

    #[test]
    fn alias_match_is_case_insensitive() {
        let aliases = vec!["пидрила".to_string()];
        assert_eq!(
            text_alias_p("ПИДРИЛА ты тут?", &aliases, "nugpuJIa", &cfg()),
            1.0
        );
    }
}
