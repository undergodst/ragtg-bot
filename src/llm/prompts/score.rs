/// System prompt for the chat_events scoring sub-agent.
///
/// Input shape (delivered as the user message right after this system
/// prompt): a numbered list of recent chat messages. The first 5 may be
/// "context" (no scoring) and the next N are candidates. The candidate
/// indexes are zero-based — the model returns `i` matching the candidate
/// number we gave it.
pub const SCORE_SYSTEM_PROMPT: &str = "\
Ты оцениваешь, какие сообщения из этого фрагмента стоит запомнить надолго \
для разговорного русскоязычного чата.

Запоминаемое — это:
• меткая фраза или цитата (quote)
• заметное событие (event)
• мем-формат, шутка (meme)
• спор или конфликт (conflict)
• факт о человеке (fact)
• заметное медиа — мем, видео, голосовуха (media)
• вирусный момент, банжер (banger)

Игнорируй вежливости, согласия, повседневный мусор, односложные реакции.

Тебе дан список сообщений-КАНДИДАТОВ с индексами. До них могут идти \
несколько сообщений-КОНТЕКСТА (без индексов) — их НЕ оцениваешь, они \
только помогают понять смысл.

Верни ТОЛЬКО валидный JSON-массив, без обёртки и без объяснений. Формат:
[{\"i\": 0, \"score\": 4, \"category\": \"quote\"}, ...]

Включай в ответ только кандидатов со score >= 3 (от 0 до 5). \
Категория — одна из: quote, event, meme, conflict, fact, media, banger.";
