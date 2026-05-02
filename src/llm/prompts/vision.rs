//! Prompts for the vision/audio sub-agent (Nemotron 3 Nano Omni and the
//! image-only fallbacks Qwen3-VL / Gemini 2.0 Flash). Each prompt assumes
//! the model emits free-form Russian text that will be stuffed into
//! working memory as `[картинка: <output>]` / `[голос: <output>]` /
//! `[кружок: <output>]`.

/// Photo / static sticker / image document.
pub const IMAGE_PROMPT: &str = "\
Опиши кратко (2-3 предложения) для контекста разговорного русскоязычного чата. \
Обращай внимание на мелкие детали, задний план и скрытые элементы. \
Если на картинке есть любой текст — приведи его дословно. \
Если это мем или известный формат — назови его. \
Без вступлений типа «на картинке изображено», сразу к сути.";

/// Voice message (`voice`) — Telegram voice notes are OGG/Opus.
pub const VOICE_PROMPT: &str = "\
Транскрибируй речь на русском. \
Затем отдельной строкой: тон (раздражённый/весёлый/нейтральный/возбуждённый/грустный) \
и заметные детали (фон, музыка, голос перебивает себя, помехи). \
Без вступлений и комментариев — только транскрипт и строка с тоном.";

/// Round video note (`video_note`) — short kruzhok, audio + face.
/// Reserved for v2 of the perception pipeline; v1 silently skips circles.
#[allow(dead_code)]
pub const CIRCLE_PROMPT: &str = "\
Транскрибируй речь на русском. \
Кратко опиши, что в кадре: где находится человек, что делает, заметные объекты вокруг. \
ВНЕШНОСТЬ человека НЕ описывай. \
Сначала транскрипт, потом строка с описанием сцены.";
