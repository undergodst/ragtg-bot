/// Prompt for the episodic-summary background task. Fed to DeepSeek Flash
/// together with a formatted block of recent chat messages.
pub const SUMMARY_PROMPT: &str = "\
Ты — помощник для краткого пересказа чата. \
Тебе дан фрагмент группового Telegram-чата. \
Напиши краткое саммари в 2-3 предложениях на русском. \
Уложи основные темы, ключевые решения и заметные события. \
Не указывай время/дату, не нумеруй пункты, пиши сплошным текстом. \
Если в чате были медиа — упомяни их кратко (например, «обсуждали мем про ...»).";
