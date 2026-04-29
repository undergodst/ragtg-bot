# Промпты для Claude Code

Используй эти промпты в Claude Code пошагово. **CLAUDE.md уже должен лежать в корне проекта** — Claude Code его подхватит автоматически.

После каждого шага — закоммить в гит и проверь что собирается (`cargo check`). Не лети вперёд пока текущий шаг не работает.

---

## ШАГ 0 — перед началом

Перед первым промптом убедись:

1. `CLAUDE.md` лежит в корне будущего проекта
2. `git init` выполнен
3. У тебя есть на руках:
   - Имя бота (или решил пока юзать `PROJECT_NAME` плейсхолдер)
   - TG bot token (от BotFather)
   - OpenRouter API key (когда починят биллинг)
   - DeepInfra API key

Если что-то из этого отсутствует — заполнишь `.env` потом, на запуске.

---

## ШАГ 1 — Каркас проекта

```
Прочитай CLAUDE.md в корне — там полная спецификация.

Сделай первый шаг — инфраструктурный каркас:

1. Cargo.toml с edition = "2024", rust-version = "1.85". Добавь все зависимости из стека (tokio, teloxide 0.13+, sqlx с features sqlite/runtime-tokio/macros/migrate, qdrant-client, deadpool-redis, reqwest с json/stream, serde, serde_json, tracing, tracing-subscriber, prometheus, figment с features toml/env, thiserror, anyhow). Используй последние стабильные версии — проверь crates.io если не уверен.

2. Скелет директорий из секции «Структура проекта». В каждом mod.rs пустой `// TODO`.

3. docker-compose.yml: три сервиса bot + qdrant + redis. Бот собирается из локального Dockerfile. Все volumes — bind mounts в ./data/. БД нет (SQLite живёт файлом в общем bind-маунте). Healthchecks на qdrant и redis. depends_on с условием service_healthy.

4. Dockerfile: multi-stage. Builder использует cargo-chef для кэша зависимостей. Runtime — debian:bookworm-slim. Бинарник копируется в /app/bot. Порты 8080 и 9090 expose. ENTRYPOINT на бинарь.

5. .env.example со всеми переменными (TG_BOT_TOKEN, OR_API_KEY, DEEPINFRA_KEY, RUST_LOG). Без значений.

6. .gitignore: target/, .env, data/, *.db, *.db-shm, *.db-wal, .DS_Store

7. config/config.toml.example по образцу из CLAUDE.md.

8. README.md с инструкцией:
   - Локальный запуск (cp .env.example .env, заполнить, mkdir -p data, docker compose up --build)
   - Прогон миграций (sqlx migrate run или через бот при старте)
   - Полезные команды (logs, restart, backup db)

9. migrations/0001_initial.sql из секции «База данных» CLAUDE.md.

НЕ пиши бизнес-логику. Только каркас.

Когда что-то непонятно — спрашивай. Не угадывай. Особенно:
- Если в crates.io нашёл два разных крейта с похожим именем — спроси какой брать
- Если не уверен в version constraint — спроси
- Если есть несовместимости между крейтами — скажи
```

---

## ШАГ 2 — Подключения к инфре + healthcheck

```
Прочитай CLAUDE.md.

Реализуй модуль storage и main с подключениями:

1. src/error.rs — общий Error через thiserror, варианты: Sqlite, Qdrant, Redis, OpenRouter, Telegram, Config, Other(anyhow).

2. src/config.rs — структуры под секции config.toml через serde::Deserialize. Загрузка через figment: TOML файл + ENV override (TG_BOT_TOKEN, OR_API_KEY, DEEPINFRA_KEY).

3. src/storage/sqlite.rs:
   - Функция init_pool(path, max_conn) → SqlitePool
   - В connect_options применить PRAGMA: journal_mode=WAL, foreign_keys=ON, synchronous=NORMAL, busy_timeout=5000
   - Функция run_migrations(&pool) через sqlx::migrate!()
   - Функция healthcheck(&pool) → Result<()> делает SELECT 1

4. src/storage/qdrant.rs:
   - Функция init_client(url) → QdrantClient
   - Функция ensure_collections(&client) — создаёт episodic_summaries, user_facts, lore, media_descriptions если не существуют. Vector size = 1024, distance = Cosine, on_disk = true. Идемпотентно.
   - Функция healthcheck — list_collections и проверка что все наши есть

5. src/storage/redis.rs:
   - Функция init_pool(url) → deadpool_redis::Pool
   - Функция healthcheck — PING

6. src/main.rs:
   - tracing_subscriber::fmt с EnvFilter из RUST_LOG
   - Загрузка конфига
   - Инициализация всех трёх стораджей параллельно через try_join
   - run_migrations
   - ensure_collections
   - axum-сервер на /healthz (port из конфига) — возвращает 200 если все три БД ок, иначе 503 с указанием что лежит
   - axum-сервер на /metrics (port из конфига) — пока заглушка, прометеевский текстовый формат

ПОКА не подключаем teloxide. Цель этого шага: запустить контейнер, увидеть что healthz отдаёт OK.

Покажи итоговый docker compose up && curl localhost:8080/healthz который должен работать.
```

---

## ШАГ 3 — Teloxide handlers + сохранение сообщений

```
Прочитай CLAUDE.md.

Подключи teloxide и реализуй базовое сохранение сообщений:

1. src/bot/mod.rs:
   - Функция build_dispatcher(bot, deps) → возвращает Dispatcher
   - Регистрирует handlers: на любые text-сообщения, на медиа-сообщения, на команды
   - Зависимости (deps: SqlitePool, RedisPool, QdrantClient, Config) прокидываются через teloxide dependency injection

2. src/bot/handlers.rs:
   - handle_message(msg, deps) — главный обработчик
   - При получении сообщения:
     a. UPSERT в users (id, username, first_name) — INSERT OR REPLACE
     b. UPSERT в chats (id, title)
     c. INSERT в messages (chat_id, user_id, tg_message_id, text, has_media)
     d. Логировать через tracing::info с полями chat_id, user_id, текст обрезать до 100 символов
   - Пока без ответов — только сохраняем

3. src/bot/commands.rs:
   - Команды через teloxide BotCommands derive: /start, /ping, /stats
   - /start — приветствие (заглушка, потом перепишем под личность)
   - /ping — отвечает «pong»
   - /stats — отдаёт count сообщений в этом чате (SELECT COUNT(*) FROM messages WHERE chat_id=?)

4. В main.rs добавь запуск диспетчера в spawn-таску. Healthz продолжает крутиться. Если диспетчер упал — логируем error, healthz возвращает 503.

5. Тестовая инструкция в README:
   - Создать бота через @BotFather, токен в .env
   - Запустить compose
   - Добавить бота в группу
   - Написать любое сообщение
   - Увидеть в логах что сохранилось
   - sqlite3 data/bot.db "SELECT * FROM messages" — увидеть запись

Покажи как протестировать локально.
```

---

## ШАГ 4 — Working memory в Redis

```
Прочитай CLAUDE.md, секция «Working memory».

Реализуй слой working memory:

1. src/memory/working.rs:
   - Структура WorkingMessage { user_id: i64, username: Option<String>, text: String, media_desc: Option<String>, ts: i64 }
   - Функция push(redis, chat_id, msg: WorkingMessage):
     - LPUSH в ключ chat:{chat_id}:window сериализованный JSON
     - LTRIM до working_window_size (из конфига)
     - EXPIRE на working_ttl_days * 86400
   - Функция get_window(redis, chat_id, n) → Vec<WorkingMessage>:
     - LRANGE 0..n
     - Десериализация
     - Возвращать в хронологическом порядке (LRANGE даёт обратный, развернуть)

2. В bot/handlers.rs handle_message: после INSERT в SQLite — вызвать working::push с собранным WorkingMessage. Медиа пока без описания (None), text только.

3. Команда /window (для дебага, доступна только админам из admin_ids конфига): показывает текущее окно для этого чата отформатированно.

4. Тест:
   - Написать 5 сообщений в чат
   - Вызвать /window — увидеть последние 5 в правильном порядке
   - Написать 35 сообщений
   - /window показывает только последние 30

Покажи как тестировать.
```

---

## ШАГ 5 — Первый ответ через DeepSeek Flash (sanity check)

```
Прочитай CLAUDE.md.

Подключи OpenRouter и сделай первый ответ:

1. src/llm/client.rs:
   - Структура OpenRouterClient { http: reqwest::Client, base_url, api_key }
   - Метод chat_completion(model, messages, max_tokens) → Result<String>
     - POST на /chat/completions с OpenAI-совместимым телом
     - Headers: Authorization: Bearer {api_key}, Content-Type: application/json, HTTP-Referer и X-Title (для OR-аналитики)
     - Парсинг content из choices[0].message.content
     - Retry на 429/5xx с exponential backoff (используй tokio::time::sleep, до max_retries из конфига)
     - Timeout из конфига
   - Структуры Message { role: String, content: ChatContent } где ChatContent либо строка, либо Vec<ContentBlock> (для будущей мультимодальности — пока только текст)

2. src/llm/prompts/system.rs:
   - Константа SYSTEM_PROMPT_BASE: пока заглушка типа «Ты — член телеграм-чата. Отвечай коротко (1-3 предложения), в разговорном стиле. Не представляйся ассистентом.». Финальная личность будет позже.

3. src/bot/handlers.rs:
   - В handle_message добавь логику ответа:
     - Если бот @mention'ed или это reply на сообщение бота — отвечаем
     - Иначе — пока не отвечаем (decision layer будет на следующих шагах)
   - При ответе:
     - Получить window через working::get_window
     - Собрать messages для LLM: [{role: system, content: SYSTEM_PROMPT_BASE}, ...window_as_user_messages, {role: user, content: новое_сообщение}]
     - Каждое сообщение из window форматируется как «{username}: {text}»
     - Вызвать client.chat_completion(model_main, messages, 300)
     - Отправить ответ через bot.send_message с reply_to_message_id

4. src/bot/handlers.rs handle_message сохраняет ответ бота тоже в messages (user_id = bot's own id) и в working memory.

5. Тест:
   - Упомянуть @your_bot в сообщении
   - Получить ответ
   - Проверить в логах токены/латенси

Покажи как протестировать и где в логах смотреть стоимость запроса (если OR возвращает в headers).
```

---

## ШАГ 6 — Vision pipeline через Nemotron

```
Прочитай CLAUDE.md, секция «Vision/audio pipeline».

Реализуй perception layer:

1. src/llm/perception.rs:
   - enum MediaType { Photo, Video, Voice, VideoNote, Document }
   - Функция describe(file_bytes: &[u8], mime: &str, kind: MediaType, deps) → Result<String>:
     - SHA256 от file_bytes
     - Проверка кэша в Redis: GET media:desc:{sha256}. Если есть — вернуть.
     - Подбор промпта по MediaType (vision.rs)
     - Вызов OpenRouter с моделью model_vision и multimodal content (image_url с base64 data URI или ссылкой, audio для голосовух)
     - На 429/5xx — fallback по списку vision_fallbacks
     - SET media:desc:{sha256} value EX (30*86400)
     - Возврат описания

2. src/llm/prompts/vision.rs — три промпта (картинка, голосовуха, кружок) дословно из CLAUDE.md.

3. В bot/handlers.rs handle_message:
   - При наличии медиа: bot.get_file → bot.download_file → bytes
   - perception::describe(...)
   - Записать description в messages.media_description (UPDATE)
   - В working memory положить с media_desc Some(description)

4. Семафор на vision: tokio::sync::Semaphore с capacity = vision_concurrent. acquire перед вызовом, release после. Если acquire висит >5 сек — логируем warn и пропускаем.

5. Тест:
   - Кинуть в чат картинку с упоминанием бота
   - Бот должен ответить «как будто видит» — то есть в его ответе будет контекст картинки
   - Кинуть голосовуху — то же
   - Повторно кинуть ту же картинку — вторая обработка должна попасть в кэш (видно по логам timing)

Покажи как тестировать и как смотреть кэш-хиты.
```

---

## ШАГ 7 — Episodic memory (саммари)

```
Прочитай CLAUDE.md, секция «Episodic memory».

Реализуй эпизодическую память:

1. src/llm/embeddings.rs:
   - Структура EmbeddingsClient { http, api_key, model }
   - Метод embed(texts: Vec<String>) → Result<Vec<Vec<f32>>> через DeepInfra POST на /v1/openai/embeddings
   - Batch до 100 текстов за вызов

2. src/llm/prompts/summary.rs — промпт для саммаризации:
   «Ниже фрагмент разговора в чате. Сделай саммари в 2-3 предложениях на русском, фокус на: о чём говорили, кто что сказал важного, события. Если был значимый момент (внутряк, мем, скандал) — отметь это словом ВАЖНО в начале строки.»

3. src/memory/episodic.rs:
   - summarize_segment(messages: &[Message]) → Result<(String, bool)>: возвращает (текст, is_lore=true если LLM пометил ВАЖНО)
   - store_summary(text, embedding, chat_id, range_start, range_end, is_lore, deps) — INSERT в SQLite + UPSERT в Qdrant с qdrant_point_id = uuid v4
   - search(query_embedding, chat_id, top_k) → Vec<SummaryHit { text, similarity, sqlite_id }>:
     - Qdrant search с filter по chat_id, top_k из конфига

4. src/tasks/summarize.rs:
   - Фоновая таска tokio::spawn в main
   - Цикл: каждые 5 минут — для каждого активного чата проверить:
     - Есть ли >= episodic_summary_every_n новых сообщений с момента последней саммари?
     - Если да — взять последние episodic_summary_lookback сообщений, summarize, store
   - Использовать Mutex/lock в Redis (SET NX EX) чтобы при множественных инстансах не дублировалось (на будущее)

5. В bot/handlers.rs при формировании контекста для ответа:
   - Эмбедить новое сообщение
   - search top_k_summaries
   - Подмешать саммари в system prompt секцией «Контекст из истории чата:\n- ...\n- ...»

6. Тест:
   - Написать в чат 25 сообщений на тему N
   - Подождать 5 минут (или вручную дёрнуть таску через /admin_summarize)
   - SELECT * FROM episodic_summaries WHERE chat_id = X — увидеть запись
   - Через несколько дней спросить бота что-то по теме N — он должен подтянуть саммари в ответ

Покажи как тестировать включая ручное триггеринг таски.
```

---

## ШАГ 8 — Semantic memory (факты о юзерах)

```
Прочитай CLAUDE.md, секция «Semantic memory».

Реализуй экстракцию фактов о юзерах:

1. src/llm/prompts/facts.rs — промпт:
   «Ниже последние сообщения от юзера {username} в чате. Извлеки факты о нём: что любит, что делает, отношения с другими, привычки, мнения. Верни JSON-массив объектов { "fact": "...", "type": "preference|history|relationship|opinion|habit", "confidence": 0.0..1.0 }. Только реальные факты, не предположения. Если ничего интересного — верни []. На русском.»

2. src/memory/semantic.rs:
   - extract_facts(user_id, chat_id, messages, deps) → Result<Vec<UserFact>>:
     - Вызов DeepSeek Flash, парсинг JSON
     - На каждый факт: эмбединг
     - Дедуп: search по Qdrant с filter user_id+chat_id, top_k=3, similarity>fact_dedup_threshold → если есть похожий, скип
     - Иначе — INSERT в SQLite + UPSERT в Qdrant
   - search_facts_for_user(user_id, chat_id, query_embedding, top_k) → Vec<UserFact>

3. src/tasks/extract_facts.rs:
   - Раз в 10 минут — для каждого активного юзера в каждом активном чате:
     - SELECT COUNT новых сообщений с last_extraction_at
     - Если >= facts_extraction_every_n — extract_facts
   - Хранить last_extraction_at в Redis: GET/SET facts:last:{user_id}:{chat_id}

4. В bot/handlers.rs при формировании контекста:
   - Для каждого юзера в текущем working window — search_facts_for_user
   - Подмешать в system prompt: «Что ты знаешь о юзерах:\n@vasya: ...\n@petya: ...»

5. Команда /forget @username (только админ): DELETE FROM user_facts WHERE user_id=? AND chat_id=? + Qdrant delete по фильтру.

6. Тест:
   - Юзер пишет 20 сообщений где упоминает что играет в Тарков и ненавидит укроп
   - Через 10 минут (или ручной триггер) — SELECT * FROM user_facts WHERE user_id=X
   - Должны быть факты «играет в Escape from Tarkov» и «не любит укроп»
   - Спросить бота про этого юзера — он должен подтянуть факты в ответ
```

---

## ШАГ 9 — Decision layer

```
Прочитай CLAUDE.md, секция «Decision layer».

Реализуй слой принятия решения отвечать или нет:

1. src/decision.rs:
   - rules_check(msg, last_bot_reply_ts, config) → f32 (вероятность):
     - mention бота → mention_p
     - reply на сообщение бота → reply_p
     - имя бота в тексте без mention → name_in_text_p
     - есть «?» в тексте И прошло > silence_threshold_min мин с последнего ответа бота → question_after_silence_p
     - default → random_p
   - llm_check(window, new_msg, deps) → bool:
     - Вызов model_decision с промптом из decision.rs prompts
     - Парсинг yes/no из ответа

2. src/llm/prompts/decision.rs — промпт:
   «Ты — фильтр. Решаешь, должен ли бот-член чата ответить на новое сообщение. Контекст последних сообщений:\n{window}\n\nНовое сообщение от {user}: {text}\n\nОтветь только yes или no. Yes — если уместно вмешаться: задан вопрос боту, провокация, шутка которую можно поддержать, явное приглашение. No — если это диалог между другими, бот будет лишним.»

3. В bot/handlers.rs handle_message:
   - После сохранения, перед генерацией ответа:
     - Прогнать rules_check → P
     - random < P? иначе скип
     - Прогнать llm_check → bool
     - Если false — скип
   - last_bot_reply_ts хранить в Redis: GET/SET bot:lastreply:{chat_id}, обновлять после ответа

4. Логировать решения через tracing::debug с полями chat_id, P_rules, llm_decision, final_decision.

5. Команда /decision_debug on/off (админ): включает в чате логирование решений в виде временных сообщений (auto-delete через 30 сек) — для отладки уместности.

6. Тест:
   - Бот не должен отвечать на каждое сообщение в чате (раньше отвечал только на mention, теперь — иногда сам)
   - В пустом чате с одним вопросом — должен отвечать чаще
   - В оживлённом обсуждении между двумя — реже
```

---

## ШАГ 10 — Lore + админские команды

```
Прочитай CLAUDE.md, секция «Lore».

Реализуй лор-систему и админ-команды:

1. src/memory/lore.rs:
   - add_lore(chat_id, text, tags, added_by, deps) → INSERT + Qdrant upsert
   - delete_lore(id, chat_id, deps)
   - list_lore(chat_id, limit, offset) → Vec<LoreEntry>
   - search_lore(query_embedding, chat_id, top_k) → Vec<LoreEntry>
   - Авто-добавление: если в saved summary is_lore=true — копируем текст в lore при сохранении (insert в обе таблицы)

2. Команды админ-only (проверка по admin_ids):
   - /lore_add текст #tag1 #tag2 — добавить запись (теги опциональны)
   - /lore_list [page] — пагинированный список
   - /lore_del {id} — удалить
   - /lore_search текст — semantic search для проверки

3. В bot/handlers.rs при формировании контекста:
   - search_lore с query_embedding нового сообщения
   - top_k_lore результатов в system prompt секцией «Лор чата:\n- ...»

4. Тест:
   - /lore_add «Васян кинул всех на 500р в декабре 2024» #кидала #васян
   - Через несколько сообщений написать «помните как нас кидали»
   - Бот должен подтянуть лор и упомянуть Васяна
```

---

## ШАГ 11 — Rate limiting

```
Прочитай CLAUDE.md, секция «Rate limiting».

Реализуй лимиты:

1. src/storage/redis.rs (или отдельный rate_limit.rs):
   - check_user_cooldown(redis, user_id) → bool:
     - SET rl:user:{user_id} 1 NX EX user_cooldown_sec
     - true если SET прошёл (юзер свободен), false если уже стоит (на cooldown)
   - check_chat_rate(redis, chat_id) → bool:
     - INCR rl:chat:{chat_id}, EXPIRE 60 при первом INCR (через PEXPIRE NX)
     - true если value <= chat_max_per_min
   - Семафор на vision уже есть из шага 6 — переиспользовать

2. В bot/handlers.rs перед llm_check (или сразу после rules_check):
   - check_user_cooldown — false → silent skip + tracing::debug
   - check_chat_rate — false → silent skip
   - При ответе — увеличить счётчики (для chat — INCR в check уже сделал)

3. Метрики Prometheus:
   - rl_user_skipped_total (counter)
   - rl_chat_skipped_total (counter)
   - rl_vision_queue_wait_seconds (histogram)

4. Тест:
   - Спам-сообщения от одного юзера каждую секунду — бот отвечает максимум раз в 30 сек
   - Если несколько юзеров шлют разом — суммарно не больше 10 ответов в минуту
```

---

## ШАГ 12 — Метрики Prometheus + структурное логирование

```
Прочитай CLAUDE.md.

Допилить наблюдаемость:

1. src/metrics.rs:
   - Метрики:
     - llm_requests_total{model, status} (counter)
     - llm_request_duration_seconds{model} (histogram)
     - llm_input_tokens_total{model} (counter)
     - llm_output_tokens_total{model} (counter)
     - vision_cache_hits_total / vision_cache_misses_total
     - episodic_summaries_created_total
     - user_facts_extracted_total
     - decision_skipped_total{reason="rules"|"llm"|"ratelimit"}
     - tg_messages_received_total
     - tg_messages_replied_total
   - Глобальный реестр через lazy_static или OnceLock

2. /metrics endpoint в main.rs — отдаёт через prometheus::TextEncoder.

3. В каждом критичном вызове — обновить соответствующую метрику.

4. Tracing:
   - Все хендлеры с #[tracing::instrument(skip(deps), fields(chat_id, user_id))]
   - Маскирование секретов: добавь sanitizer для api_key в логах
   - При ответе LLM — логировать input_tokens, output_tokens, latency_ms

5. README дополни инструкцией: как смотреть метрики (curl localhost:9090/metrics) и логи (docker compose logs -f bot | grep -i llm).
```

---

## ШАГ 13 — Личность бота (отдельный итеративный шаг)

Это **не один промпт, а серия итераций**. Личность вырабатывается на живых данных.

### Старт:

```
Прочитай CLAUDE.md, секция «Личность бота».

Перепиши src/llm/prompts/system.rs.

Базовый системный промпт должен быть на русском, ~300-500 слов. Включает:
1. Кто бот: член чата, не ассистент
2. Стиль речи: краткий, разговорный, может матерится в меру, 2ch-ирония
3. Что НЕ делать: не предлагать помощь, не задавать «чем могу помочь», не извиняться, не делать списков и заголовков, не использовать markdown в ответах, не подражать ChatGPT
4. Поведение под провокациями: отвечать в тон, не вылетать из роли
5. Длина ответов: 1-3 предложения максимум, можно одним словом

Также реализуй подмешивание few-shot примеров:
- src/llm/prompts/examples/ — папка с *.json файлами
- Каждый файл: { "context": "Васян: ...\nПетя: ...", "response": "..." }
- При сборке system prompt — подмешать 5 случайных примеров в формате «Контекст: ...\nТы ответил: ...»

Создай 10 стартовых примеров в examples/. На основе принципов выше. Без перебора с матом — он должен быть редким и в тему.
```

### Дальнейшие итерации:

1. Запусти бота, неделю собирай удачные/неудачные ответы
2. Удачные → добавляй в `examples/` как новые JSON
3. Неудачные → правь системный промпт (ужесточай нужные ограничения)
4. Раз в месяц — рефакторинг: убирай примеры которые перестали работать, добавляй свежие

---

## ШАГ 14 — Бэкапы

```
Прочитай CLAUDE.md.

Реализуй бэкап-систему:

1. scripts/backup.sh:
   - SQLite: sqlite3 data/bot.db ".backup data/backups/bot.$(date +%F-%H%M).db"
   - Qdrant: curl POST localhost:6333/collections/{collection}/snapshots для каждой коллекции, затем cp снапшота в data/backups/qdrant/
   - Ротация: rm файлы старше 30 дней
   - Логирование в data/backups/backup.log

2. README дополни:
   - Cron на хосте: 0 3 * * * cd /path/to/bot && ./scripts/backup.sh
   - Восстановление: инструкция как остановить бота, заменить bot.db, восстановить snapshot Qdrant

3. Альтернатива — в Compose добавь сервис `backup` на базе alpine с cron'ом, который раз в сутки гонит backup.sh. Volume — общий с ./data/.
```

---

## Хвост: что после всех 14 шагов

1. Реальный продакшен-тест: добавь бота в свой основной чат
2. Неделя наблюдения, фиксация проблем
3. Итерации по личности (шаг 13 без конца)
4. Возможные расширения:
   - Reactions API: бот ставит эмодзи-реакции вместо текстовых ответов в part случаев
   - Голосовые ответы: TTS через ElevenLabs/Coqui, иногда отвечает голосовухой
   - Автоматические подсветки моментов чата в саммари (типа daily digest)
   - Web-интерфейс для просмотра и редактирования lore/facts (минимальный axum + htmx)

---

## Шпаргалка по работе с Claude Code

- **Не давай Claude писать сразу всё.** Маленькие шаги, проверка после каждого.
- **CLAUDE.md — источник истины.** Если меняешь решение — правь его сразу же.
- **После каждого шага: `cargo check`, `cargo clippy`, git push.**
- **Если Claude галлюцинирует API крейта** (например, неправильное имя метода у teloxide или sqlx) — попроси «проверь docs.rs последней версии {крейт} и исправь».
- **Если код собирается но не работает** — проси Claude писать `tracing::debug` логи в подозрительных местах, перезапускай, читай логи.
- **Не доверяй Claude в части секретов и безопасности** — проверяй что API-ключи не попадают в логи и в commit'ы.
- **Версии крейтов** — раз в неделю `cargo update` + проверка что собирается.

Удачи. Главное — не забудь финализировать имя бота и личность, без этого бот будет generic.
