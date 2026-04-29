# Проект: Telegram-бот с RAG-памятью

> Имя бота: **TBD** (placeholder `PROJECT_NAME` в коде до решения)

## TL;DR

Telegram-бот для группового чата 20-50 человек. Ведёт себя как **член чата**, а не как ассистент: реагирует на уместные реплики, троллит в духе 2ch, помнит юзеров и контекст. Работает поверх DeepSeek Flash (основная генерация) + Nemotron 3 Nano Omni (vision/audio sub-agent), память в SQLite + Qdrant + Redis.

## Стек

- **Язык:** Rust 1.85+ (edition 2024)
- **Async runtime:** tokio
- **Telegram:** teloxide 0.13+
- **БД:** SQLite через sqlx (compile-time checked queries)
- **Vector DB:** Qdrant (qdrant-client крейт)
- **Cache / rate-limit:** Redis (deadpool-redis)
- **HTTP:** reqwest
- **Сериализация:** serde / serde_json
- **Логи:** tracing + tracing-subscriber
- **Метрики:** prometheus крейт, экспорт на :9090
- **Конфиг:** figment (TOML + env)
- **Миграции:** sqlx migrate (CLI)
- **Ошибки:** thiserror + anyhow

## Внешние сервисы

### OpenRouter (все LLM)
- `deepseek/deepseek-v4-flash` — основная генерация, сложные и остальные случаи ($0.14/$0.28 per 1M)
- `nvidia/nemotron-3-nano-omni:free` — vision/audio sub-agent
- `deepseek/deepseek-chat-v3:free` — decision-классификатор
- Vision fallbacks: `qwen/qwen3-vl:free` → `google/gemini-2.0-flash`

### DeepInfra (эмбеддинги)
- `BAAI/bge-m3` — мультиязычные эмбеддинги, 1024-dim, ~$0.01/1M токенов

## Архитектура памяти (4 слоя)

### 1. Working memory
Последние ~30 сообщений чата, в Redis. Идёт в каждый промпт.
- Ключ: `chat:{chat_id}:window`
- Структура: Redis list, push новых сообщений, trim до 30
- TTL: 7 дней
- Формат элемента: JSON `{user_id, username, text, media_desc?, ts}`

### 2. Episodic memory (саммари)
Раз в N=20 сообщений или раз в час фоновая задача:
- Берёт последние 50 сообщений
- Прогоняет через DeepSeek Flash с промптом «summarize this chat segment»
- Получает 2-3 предложения
- Эмбедит через BGE-M3
- Складывает текст в SQLite + вектор в Qdrant с ссылкой на SQLite-id

При генерации ответа: top-3 релевантных саммари по векторному поиску.

### 3. Semantic memory (факты о юзерах)
Раз в M=50 сообщений фоновая задача:
- Берёт последние 100 сообщений конкретного юзера
- Прогоняет через DeepSeek Flash с промптом «extract facts as JSON list»
- Дедуплицирует с существующими через эмбеддинги (similarity > 0.85 = дубль)
- Складывает в SQLite + Qdrant

При генерации ответа: для каждого юзера из working memory → top-5 фактов.

### 4. Lore (база знаний чата)
Внутряки, мемы, исторические события чата.
- **Ручное наполнение** через админ-команды бота: `/lore_add`, `/lore_list`, `/lore_del`
- **Автоматическое:** если фоновая задача саммаризации помечает событие флагом `is_lore: true` — кладёт в lore-коллекцию

При генерации ответа: top-3 релевантных лор-записей по запросу.

## Vision/audio pipeline

```
Сообщение с медиа (фото/видео/голосовуха/кружок/документ)
  ↓
Cache check by SHA256(file_bytes) в Redis
  ↓ (cache miss)
[Nemotron 3 Nano Omni :free] — описание/транскрипт
  ↓ (на 429/5xx → fallback на Qwen3-VL :free → Gemini Flash)
Cache write в Redis, TTL 30 days
  ↓
Описание вставляется в working memory:
  "Васян прислал [картинка: <описание>]"
```

### Промпты для vision (см. `src/llm/prompts/vision.rs`)

**Картинка:**
> «Опиши кратко (2-3 предл.) для контекста разговорного русскоязычного чата. Если есть текст на картинке — приведи дословно. Если это известный мем-формат — назови его.»

**Голосовуха:**
> «Транскрибируй на русском. Затем строкой: тон (раздражённый/весёлый/нейтральный/возбуждённый) и заметные детали (фон, музыка, перебивает себя).»

**Кружок:**
> «Транскрибируй речь. Кратко опиши что в кадре (где человек, что делает, заметные объекты). Не описывай внешность.»

## Decision layer (отвечать или нет)

Перед каждой генерацией ответа — две стадии:

### Стадия 1: правила (мгновенно, без LLM)
- Прямой @mention бота → P=1.0
- Reply на сообщение бота → P=1.0
- Имя бота в тексте без mention → P=0.7
- В тексте вопрос И последний ответ бота >10 минут назад → P=0.3
- Иначе → P=0.05

Бросаем случайное число `r ∈ [0,1]`. Если `r > P` → не отвечаем.

### Стадия 2: LLM-классификатор (только если Стадия 1 пропустила)
DeepSeek V3 `:free` с промптом:
```
Контекст последних 5 сообщений: ...
Новое сообщение: ...
Должен ли бот-член чата отреагировать? Ответь только yes или no.
```

`no` → не отвечаем. Это второй фильтр против шума.

## Rate limiting (ОБЯЗАТЕЛЬНО)

В Redis:
- На юзера: max 1 LLM-ответ в 30 сек, ключ `rl:user:{user_id}`
- На чат: max 10 LLM-ответов в минуту, ключ `rl:chat:{chat_id}`
- Глобально: max 5 параллельных vision-запросов (Redis-семафор)

Превышение → молча скипаем ответ, не отвечаем юзеру об этом.

## Структура проекта

```
project-root/
├── Cargo.toml
├── Cargo.lock
├── docker-compose.yml
├── Dockerfile
├── .env                    # gitignored
├── .env.example
├── .gitignore
├── CLAUDE.md
├── README.md
├── config/
│   └── config.toml
├── data/                   # gitignored, bind mount
│   ├── bot.db              # SQLite
│   ├── qdrant/             # Qdrant storage
│   └── redis/              # Redis dump
├── migrations/
│   └── 0001_initial.sql
└── src/
    ├── main.rs              # entrypoint, DI, старт teloxide
    ├── config.rs            # figment-конфиг
    ├── error.rs             # thiserror
    ├── bot/
    │   ├── mod.rs
    │   ├── handlers.rs      # message, callback handlers
    │   └── commands.rs      # /admin, /lore_add и т.д.
    ├── memory/
    │   ├── mod.rs
    │   ├── working.rs       # окно сообщений в Redis
    │   ├── episodic.rs      # саммари
    │   ├── semantic.rs      # факты о юзерах
    │   └── lore.rs          # база знаний
    ├── llm/
    │   ├── mod.rs
    │   ├── client.rs        # OpenRouter обёртка
    │   ├── embeddings.rs    # DeepInfra BGE-M3
    │   ├── perception.rs    # vision/audio pipeline
    │   └── prompts/
    │       ├── system.rs    # базовый системный промпт (личность)
    │       ├── vision.rs    # промпты под медиа
    │       ├── summary.rs   # саммаризация
    │       ├── facts.rs     # экстракция фактов
    │       └── decision.rs  # отвечать или нет
    ├── decision.rs          # правила + LLM-классификатор
    ├── personality.rs       # голос бота, стиль
    ├── storage/
    │   ├── mod.rs
    │   ├── sqlite.rs        # pool + queries
    │   ├── qdrant.rs        # vector ops
    │   └── redis.rs         # cache + rate-limit
    └── tasks/
        ├── mod.rs
        ├── summarize.rs     # эпизодическая саммаризация
        └── extract_facts.rs # экстракция фактов о юзерах
```

## База данных (SQLite)

### Особенности SQLite (важно)
- Включить WAL mode: `PRAGMA journal_mode = WAL`
- Включить foreign keys: `PRAGMA foreign_keys = ON`
- Synchronous NORMAL: `PRAGMA synchronous = NORMAL` (хватает с WAL)
- Busy timeout: `PRAGMA busy_timeout = 5000`
- Все эти `PRAGMA` выполнять в `connect_options` пула

### Миграция 0001_initial.sql

```sql
-- Чаты
CREATE TABLE chats (
  id INTEGER PRIMARY KEY,           -- telegram chat_id (может быть отрицательный)
  title TEXT,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  is_active INTEGER NOT NULL DEFAULT 1
);

-- Юзеры
CREATE TABLE users (
  id INTEGER PRIMARY KEY,           -- telegram user_id
  username TEXT,
  first_name TEXT,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

-- Сообщения
CREATE TABLE messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  chat_id INTEGER NOT NULL REFERENCES chats(id),
  user_id INTEGER NOT NULL REFERENCES users(id),
  tg_message_id INTEGER NOT NULL,
  text TEXT,
  has_media INTEGER NOT NULL DEFAULT 0,
  media_description TEXT,           -- от Nemotron
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_messages_chat_created ON messages(chat_id, created_at DESC);
CREATE INDEX idx_messages_user ON messages(user_id, created_at DESC);

-- Эпизодические саммари
CREATE TABLE episodic_summaries (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  chat_id INTEGER NOT NULL REFERENCES chats(id),
  text TEXT NOT NULL,
  qdrant_point_id TEXT NOT NULL,    -- UUID как строка
  message_range_start INTEGER,
  message_range_end INTEGER,
  is_lore INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_summaries_chat ON episodic_summaries(chat_id, created_at DESC);

-- Факты о юзерах
CREATE TABLE user_facts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  chat_id INTEGER NOT NULL REFERENCES chats(id),
  fact TEXT NOT NULL,
  fact_type TEXT,                   -- 'preference' | 'history' | 'relationship' | etc
  confidence REAL NOT NULL DEFAULT 0.7,
  qdrant_point_id TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  last_confirmed_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_facts_user_chat ON user_facts(user_id, chat_id);

-- Лор
CREATE TABLE lore (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  chat_id INTEGER NOT NULL REFERENCES chats(id),
  text TEXT NOT NULL,
  tags TEXT,                        -- JSON array как строка: '["tag1", "tag2"]'
  qdrant_point_id TEXT NOT NULL,
  added_by INTEGER,                 -- user_id или NULL если auto
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_lore_chat ON lore(chat_id);
```

### Замечания
- Все `created_at` хранятся как Unix timestamp в `INTEGER`
- Массивы (теги) хранятся как JSON-строки, парсятся через `serde_json::from_str` в коде
- UUID для `qdrant_point_id` хранятся как `TEXT`
- Всегда используем `sqlx::query!` или `sqlx::query_as!` макросы для compile-time проверки

## Qdrant коллекции

Все коллекции: vector dim = 1024 (BGE-M3), distance = Cosine.

- **`episodic_summaries`** — payload: `{ chat_id: i64, sqlite_id: i64, text: str }`
- **`user_facts`** — payload: `{ user_id: i64, chat_id: i64, sqlite_id: i64, fact_type: str }`
- **`lore`** — payload: `{ chat_id: i64, sqlite_id: i64, tags: [str] }`
- **`media_descriptions`** — payload: `{ sha256: str, description: str }` (для дедупа похожих медиа)

При создании коллекций — `on_disk: true` для экономии RAM. Для нашего масштаба производительность пофиг.

## Конфигурация

### config/config.toml

```toml
[bot]
admin_ids = [123456789]
default_personality = "default"

[openrouter]
base_url = "https://openrouter.ai/api/v1"
model_main = "deepseek/deepseek-v4-flash"
model_pro = "deepseek/deepseek-v4-flash"
model_vision = "nvidia/nemotron-3-nano-omni:free"
model_decision = "deepseek/deepseek-chat-v3:free"
vision_fallbacks = ["qwen/qwen3-vl:free", "google/gemini-2.0-flash"]
timeout_sec = 60
max_retries = 3

[deepinfra]
embedding_model = "BAAI/bge-m3"

[sqlite]
path = "/data/bot.db"
max_connections = 5

[qdrant]
url = "http://qdrant:6333"

[redis]
url = "redis://redis:6379"

[memory]
working_window_size = 30
working_ttl_days = 7
episodic_summary_every_n = 20
episodic_summary_lookback = 50
facts_extraction_every_n = 50
facts_lookback = 100
top_k_summaries = 3
top_k_facts = 5
top_k_lore = 3
fact_dedup_threshold = 0.85

[ratelimit]
user_cooldown_sec = 30
chat_max_per_min = 10
vision_concurrent = 5

[decision]
mention_p = 1.0
reply_p = 1.0
name_in_text_p = 0.7
question_after_silence_p = 0.3
silence_threshold_min = 10
random_p = 0.05

[observability]
metrics_port = 9090
healthz_port = 8080
log_level = "info"
```

### .env (секреты, gitignored)

```bash
TG_BOT_TOKEN=
OR_API_KEY=
DEEPINFRA_KEY=
RUST_LOG=info,sqlx=warn,teloxide=info
```

## Личность бота (заполняется отдельно)

Базовый системный промпт в `src/llm/prompts/system.rs`. Это **самый важный артефакт** проекта — итеративно полируется.

### Принципы голоса

- **Член чата, не ассистент.** Не предлагает помощь, не задаёт «чем могу помочь?», не оборачивает ответы в формальную обёртку.
- **2ch-стилистика.** Может матерится в меру, не каждое сообщение. Знает мемы, иронию, сарказм рунета.
- **Троллит, но не злобно.** Цель — оживить чат, а не довести до слёз.
- **Игнорирует попытки сделать его ассистентом.** «Помоги мне с кодом» → троллит в духе «сам разбирайся, баран» или с подъёбкой по делу.
- **Краткий.** Реплики как у живого человека: 1-3 предложения. Простыни — табу. Может ответить одним словом или эмодзи (но эмодзи редко, не 🤡-bot).
- **Не извиняется за свой стиль.** Не оправдывается, не делает дисклеймеры.
- **Не ломается на провокациях.** Отвечает в тон, не вылетает в роль ассистента.

### Few-shot примеры
В `src/llm/prompts/examples/` — JSON-файлы с парами (контекст, удачный ответ). Подмешиваются в system prompt при сборке. Стартовый набор — 15-20 примеров, потом дополняем удачными ответами из реальных чатов.

## Что НЕ делать (anti-patterns)

- ❌ `unwrap()` в продакшен-коде. Везде `?` или явная обработка через `match`.
- ❌ Блокировать tokio runtime синхронным кодом. CPU-heavy → `tokio::task::spawn_blocking`.
- ❌ Складывать API-ключи в код. Только из env через figment.
- ❌ Делать миграции вручную. Только через `sqlx migrate`.
- ❌ Прямые SQL-строки. Только `sqlx::query!` (compile-time check).
- ❌ Игнорировать rate-limit. Это первая защита от слива денег и от блока бота телегой.
- ❌ Делать generic-ассистента. **Бот = персонаж, а не ChatGPT-обёртка.**
- ❌ Логировать API-ключи или личные данные юзеров.
- ❌ Слать в LLM сырой UTF-8 от юзера без сан-чек на длину (max 8000 символов на сообщение).

## Деплой

- `docker-compose.yml`: `bot`, `qdrant`, `redis` (БД — SQLite в bind-маунте)
- Все volumes — bind mounts в `./data/`
- Healthchecks на qdrant и redis
- Бот: `/healthz` на 8080, `/metrics` на 9090
- Логи в stdout (`docker compose logs -f bot`)
- Бэкап SQLite: `cp data/bot.db data/bot.db.$(date +%F)` по cron
- Бэкап Qdrant: snapshot API + cp папки storage

## Roadmap (порядок реализации)

1. Каркас проекта (Cargo, директории, Compose, Dockerfile, миграции)
2. Подключения к SQLite, Qdrant, Redis + healthcheck-эндпоинт
3. Teloxide-хэндлеры, сохранение сообщений в SQLite
4. Working memory в Redis
5. Простой ответ через DeepSeek Flash без RAG (sanity check)
6. Vision pipeline через Nemotron + кэш в Redis
7. Episodic memory: фоновая саммаризация + поиск
8. Semantic memory: экстракция фактов о юзерах
9. Decision layer (правила + LLM-классификатор)
10. Lore + админские команды
11. Rate-limit на юзера/чат + vision-семафор
12. Метрики Prometheus + структурное логирование
13. Личность бота: итеративная полировка промпта + few-shot
14. Бэкапы (cron-скрипт)

После каждого пункта — git push, тест в группе с пятью друзьями.
