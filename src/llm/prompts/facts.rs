/// Prompt for extracting facts about a specific user from their messages.
/// Fed to DeepSeek Flash together with a block of the user's recent messages.
///
/// The LLM should return a JSON array of objects with `fact` and `fact_type`.
pub const FACTS_PROMPT: &str = "\
Ты — аналитик, извлекающий факты о конкретном пользователе из чата. \
Тебе даны последние сообщения этого пользователя. \
Извлеки конкретные, полезные факты о нём: предпочтения, привычки, \
работа, отношения с другими участниками, интересы, мнения. \
НЕ извлекай банальности (\"пишет сообщения в чат\", \"использует русский язык\"). \
Если фактов нет — верни пустой массив.\n\
Ответь ТОЛЬКО валидным JSON-массивом объектов, без пояснений:\n\
[{\"fact\": \"описание факта\", \"fact_type\": \"тип\"}]\n\
Допустимые fact_type: preference, history, relationship, opinion, skill, habit, other.";
