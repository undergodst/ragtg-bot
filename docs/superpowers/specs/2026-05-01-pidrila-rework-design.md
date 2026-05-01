# Пидрила-rework: спецификация

**Дата:** 2026-05-01
**Статус:** утверждённый дизайн, ожидает плана реализации
**Скоуп:** личность бота, динамический сбор промпта, замена ручного лора авто-памятью моментов, фикс vision-пайпа, упрощение decision-слоя.

---

## 1. Цели

1. **Личность.** Заменить безымянный «бот-троль с правилами» на конкретного персонажа (Пидрила) с сильным характером и стабильным голосом.
2. **Динамический промпт.** Перейти от единого статического промпта к 7-слойному билдеру, который собирает контекст под каждое сообщение и максимально использует prompt-cache OpenRouter.
3. **Авто-память моментов.** Заменить мёртвый ручной лор на коллекцию `chat_events` — поток значимых моментов, попадающих туда автоматически через эвристический фильтр + LLM-скорер.
4. **Vision-фикс.** Перенести описание медиа в persist-путь (описывать всё, не только то, на что бот отвечает), запинить рабочую vision-модель, переписать image-промпт с приоритетом на распознавание текста на картинке.
5. **Decision-фикс.** Убрать рандомные ответы и LLM-классификатор: бот отвечает только когда к нему обратились (mention/reply/alias).

## 2. Не-цели

- Видео-кружки и видео-ролики (нужен ffmpeg для извлечения аудио — отдельной фичей).
- Динамическая адаптация тона под настроение чата (вариант C из брейнсторма — позже как слой поверх).
- Многоязычная поддержка — текущий упор на русскоязычный чат.
- Миграция данных из старой `lore` таблицы (она пустая на проде).

---

## 3. Личность — Пидрила

**Архетип** (живёт в `src/llm/prompts/persona.rs` как `CORE_PERSONA`):

> Пидрила. Лет 30, бывший айтишник, выгорел и осел в этом чате. Целыми днями торчит онлайн, никуда не спешит. Олдфаг рунета лурк-эры, помнит мемы, которых вы не знали. Циничный, но не озлобленный — наблюдатель, который видит насквозь. Презирает корпоративный новояз, душнил, гипстеров и тех, кто «продаёт курсы». Уважает чёрный юмор, точные подъёбки, людей, которые сами разобрались. Если кто-то реально в беде — может неожиданно поддержать, но не теплыми словами, а метким наблюдением. Не ассистент. Если просят писать код — троллит без злобы. Голос: коротко (1-3 предложения), в точку, без markdown, эмодзи редко, мат к месту.

**Few-shot пул** — `src/llm/prompts/examples/shots.json` расширяется до 25-30 примеров с тегами категорий (`code_request`, `provocation`, `casual`, `meme`, `support`, `meta`). Билдер выбирает топ-3 по семантической близости к новому сообщению.

---

## 4. Динамический prompt-builder

**Модуль:** `src/llm/prompt_builder.rs`. Точка входа: `assemble(deps, chat_id, user_msg, window) -> Vec<Message>`.

**Слои (в порядке сборки):**

| Слой | Когда обновляется | Размер | Назначение |
|---|---|---|---|
| `CORE_PERSONA` | статика, кэшируется | ~250 ток | Кто такой Пидрила, голос, базовые принципы |
| `CHAT_DNA` | фоном раз в сутки на чат | ~80 ток | «Это чат таких-то с такой темой» — синтезируется LLM из последних 20 саммари, кэшируется в Redis с TTL 24h |
| `CHAT_EVENTS_RAG` | per-message, top-K=5 | переменно | Релевантные моменты из `chat_events` по cosine |
| `EPISODIC_RAG` | per-message, top-K=2 | переменно | Релевантные саммари |
| `PEOPLE_IN_ROOM` | per-message | до ~5 юзеров × 3 факта | Что бот знает о людях из working-window |
| `STYLE_EXAMPLES` | per-message, top-K=3 | ~150-300 ток | Few-shot, выбранные по близости к новому сообщению |
| `ACTIVE_THREAD` | per-message | до 30 сообщений | Working window + новое сообщение; реплики бота помечены `[я]:` |

**Кэширование:** `CORE_PERSONA` подаётся в OpenRouter с маркером кэша; статичность гарантирует cache-hit на 60-80% input-токенов после первого запроса.

**Исчезает:** статический `FULL_SYSTEM_PROMPT` из `src/llm/prompts/system.rs` (тело переезжает в `persona.rs` как `CORE_PERSONA`).

---

## 5. `chat_events` — авто-память моментов

### 5.1 Хранилище

**SQLite** — миграция `migrations/0002_chat_events_and_drop_lore.sql`:

```sql
CREATE TABLE chat_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  chat_id INTEGER NOT NULL REFERENCES chats(id),
  source_message_id INTEGER REFERENCES messages(id),
  text TEXT NOT NULL,
  category TEXT NOT NULL,
  score INTEGER NOT NULL,
  qdrant_point_id TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_chat_events_chat ON chat_events(chat_id, created_at DESC);
CREATE INDEX idx_chat_events_category ON chat_events(category, created_at DESC);

DROP TABLE IF EXISTS lore;
```

**Qdrant:** новая коллекция `chat_events`, dim=1024, on_disk, distance Cosine. Старая коллекция `lore` удаляется на старте (`delete_collection("lore").ok()`).

**Payload:** `{ chat_id: i64, user_id: i64, sqlite_id: i64, category: str, score: u8, ts: i64 }`.

**Категории:** `quote`, `event`, `meme`, `conflict`, `fact`, `media`, `banger`.

### 5.2 Двухступенчатый шумофильтр

**Ступень 1 — эвристики** (`src/memory/events.rs::is_candidate`, на каждом сохранённом сообщении, бесплатно):

- Длина текста (без пробелов) ≥ 15 символов **ИЛИ** есть `media_description`.
- Не команда (не начинается с `/`).
- Не пустые эмодзи/стикер без описания.
- Не точный дубль одной из последних 50 строк чата (hash-сравнение в Redis-set с TTL 1 час).

Что прошло — кладётся в Redis-список `chat:{id}:event_candidates` (TTL 1d).

**Ступень 2 — LLM-скорер** (`src/tasks/score_events.rs`, триггерится когда буфер достигает N=15):

- Вытягиваются 15 кандидатов + 5 предыдущих сообщений как контекст (без скоринга).
- Один батч-запрос к **дешёвой `:free` модели** (`nvidia/nemotron-3-super-120b-a12b:free` или равнозначная).
- Промпт `src/llm/prompts/score.rs`:
  ```
  Ты оцениваешь, какие сообщения из этого фрагмента стоит запомнить надолго.
  Запоминаемое — это:
  • меткая фраза/цитата (quote)
  • заметное событие (event)
  • мем-формат, шутка (meme)
  • спор/конфликт (conflict)
  • факт о человеке (fact)
  • заметное медиа (media)
  • вирусный момент / банжер (banger)

  Игнорируй вежливости, согласия, повседневный мусор.
  Верни ТОЛЬКО JSON:
  [{"i": 0, "score": 0-5, "category": "..."}, ...]
  Только сообщения со score >= 3.
  ```
- Score ≥ 3 → батч-эмбеддинг (один HTTP к BGE-M3 на все выжившие) → запись в SQLite + Qdrant.
- Если LLM упал/таймаут — кандидаты остаются в буфере, повторим на следующем триггере.

**Ступень 3 — фоновый дедуп** (`src/tasks/dedup_events.rs`, `tokio::interval(86400s)`):

- На активный чат: последние 1000 событий.
- Группировка по `category`. Внутри группы — попарное сравнение через Qdrant; similarity > 0.92 → оставляем выше score (при равенстве — старшее по `created_at`). Лишние удаляются и из SQLite, и из Qdrant.

### 5.3 Чтение

`src/memory/events.rs::retrieve_relevant`:

- Вход: chat_id, query_vector.
- Top-K=`memory.top_k_events` (default 5) с фильтром `chat_id`.
- Возвращает `Vec<String>` — текст событий из SQLite.

Билдер дёргает это для слоя `CHAT_EVENTS_RAG`.

---

## 6. Vision rework

### 6.1 Перенос в persist-путь

**Сейчас:** `perceive_media` зовётся в `reply()` (`handlers.rs:217`). Большинство медиа никогда не описываются.

**После:** в `save_message_handler`, после persist в SQLite, при `has_media=true` спавним детач-таск:

```rust
tokio::spawn(async move {
    let _slot = rl::acquire_vision_slot(&deps.redis, max_slots).await;
    let bytes = download_media(&bot, &msg).await?;
    let sha = sha256(&bytes);
    let desc = match cache_lookup(&deps.redis, &sha).await? {
        Some(d) => d,
        None => {
            let d = perception::describe(&deps.openrouter, &bytes, mime).await?;
            cache_write(&deps.redis, &sha, &d, 30 * DAY).await?;
            d
        }
    };
    sqlx::query!("UPDATE messages SET media_description = ? WHERE id = ?", desc, msg_id).execute(...).await?;
    working::patch_media_desc(&deps.redis, chat_id, tg_message_id, &desc).await?;
});
```

**Вытекающие изменения:**
- `reply()` больше не вызывает `perceive_media`. Читает `media_description` из SQLite/working window как готовый факт.
- `working::patch_media_desc` — новый метод: находит запись в Redis-list по `tg_message_id` и обновляет JSON.
- Описание автоматически становится кандидатом в `chat_events` через `is_candidate` (длина проходит).

### 6.2 Модели

В `config/config.toml.example`:

```toml
model_vision = "google/gemini-2.5-flash"
vision_fallbacks = ["qwen/qwen3-vl-30b-a3b:free", "meta-llama/llama-4-scout:free"]
```

Перед мержем — проверить актуальность ID моделей через OpenRouter `/models` API (`scratch/find_vision_models.py` уже есть как стартовая точка). Сами скрач-скрипты в коммит этой фичи не идут — проверка ручная.

### 6.3 Промпт

`src/llm/prompts/vision.rs::IMAGE_PROMPT` переписывается:

```
Опиши картинку для русскоязычного чата.

ПРИОРИТЕТЫ (именно в этом порядке):
1) Любой текст на картинке — приведи ДОСЛОВНО, в кавычках.
2) Если это известный мем-формат (Дрейк, Отвлечённый парень, Трольфейс,
   двачерский шаблон и т.п.) — назови формат и кто что говорит.
3) Сцена: что/кто на ней, ключевые объекты, эмоция, фон.

3-4 коротких строки. Без «на изображении видно».
```

`VOICE_PROMPT` остаётся.

### 6.4 Стикеры и видео

- **Статические стикеры** (image/webp без анимации) — обрабатываются как картинки.
- **Анимированные/видео-стикеры** (`tgs`, `webm`) — без vision; пишем `[стикер из набора "<set_name>"]`.
- **Кружки и видео** — `[видео-кружок длиной Xс]` без описания. Обработка вне скоупа этого захода.

---

## 7. Decision-слой — режем рандом

### Сейчас

Стадия 1 (`src/decision.rs::stage1_probability`):
```
mention                 P=1.0
reply                   P=1.0
alias_in_text           P=0.7
question_after_silence  P=0.3
random                  P=0.05
```

Стадия 2: LLM-классификатор (`prompts/decision.rs`).

### После

Стадия 1 упрощается:
```
mention            P=1.0    (@nugpuJIa_bot)
reply              P=1.0    (реплай на сообщение бота)
alias_in_text      P=1.0    (одно из bot.aliases в тексте)
иначе              P=0.0
```

Стадия 2 удаляется. `prompts/decision.rs` удаляется. `model_decision` из конфига удаляется.

**Следствие:** бот отвечает строго когда к нему обратились. Никаких рандомных вклиниваний.

---

## 8. Конфиг — изменения

`config/config.toml.example`:

**Добавляется:**
```toml
[memory]
top_k_events = 5
event_min_chars = 15
event_scorer_batch = 15
event_score_threshold = 3
event_dedup_similarity = 0.92
event_dedup_interval_hours = 24
chat_dna_refresh_hours = 24

[openrouter]
model_vision = "google/gemini-2.5-flash"
vision_fallbacks = ["qwen/qwen3-vl-30b-a3b:free", "meta-llama/llama-4-scout:free"]
```

**Удаляется:**
- `[openrouter] model_decision`
- `[decision] question_after_silence_p`
- `[decision] silence_threshold_min`
- `[decision] random_p`

**Меняется:**
- `[decision] name_in_text_p` → `alias_in_text_p`, default `1.0`.

`src/config.rs` — синхронизировать структуру с TOML (добавить новые поля, выпилить старые).

---

## 9. Метрики (Prometheus)

Новые counters:

```
events_scored_total{category}
events_stored_total
events_dedup_dropped_total
vision_describe_total
vision_cache_hit_total
vision_fallback_used_total{model}
decision_skip_total{reason="none"}     # для отладки молчания
prompt_assembly_ms                      # histogram
```

Старое снимаем:
- `decision_llm_classifier_total` — стадии 2 больше нет.

---

## 10. Файловая дельта (для плана реализации)

**Удаляются:**
- `src/memory/lore.rs`
- `src/llm/prompts/decision.rs`
- Команды `LoreAdd`, `LoreList`, `LoreDel` в `src/bot/commands.rs`

**Новые:**
- `src/memory/events.rs` — запись/чтение `chat_events`, `is_candidate`
- `src/llm/prompt_builder.rs` — динамическая сборка
- `src/tasks/score_events.rs` — LLM-скорер
- `src/tasks/dedup_events.rs` — фоновый дедуп
- `src/llm/prompts/persona.rs` — `CORE_PERSONA`
- `src/llm/prompts/score.rs` — промпт скорера
- `migrations/0002_chat_events_and_drop_lore.sql`

**Существенно меняются:**
- `src/bot/handlers.rs` — perception в persist-пути; reply через prompt_builder; снести вызовы `perceive_media` из reply
- `src/llm/perception.rs` — пины моделей, новый image-промпт
- `src/llm/prompts/system.rs` — статический `FULL_SYSTEM_PROMPT` удаляется (логика переезжает в builder)
- `src/llm/prompts/examples/shots.json` — расширение до 25-30, теги категорий
- `src/llm/prompts/summary.rs` — добавить инструкцию выделять lore-достойные моменты
- `src/decision.rs` — упрощение
- `src/memory/working.rs` — добавить `patch_media_desc(tg_message_id)`
- `src/storage/qdrant.rs` — `COLLECTIONS` обновить, добавить `delete_collection("lore")` на старте
- `config/config.toml.example` + `src/config.rs` + `src/main.rs` — новые ключи

---

## 11. Тестирование

**Unit:**
- `events::is_candidate` — табличка кейсов (короткое/длинное, дубль, команда, эмодзи, стикер с описанием).
- `prompt_builder::assemble` — детерминированность при одинаковых входах; порядок слоёв; усечение при переполнении.
- `decision::p_for` — три триггера дают `P=1.0`, всё остальное `0.0`.
- `score_events::parse_response` — устойчивость к мусору в JSON-ответе LLM.

**Интеграционные:**
- В docker-compose поднимаем SQLite + Qdrant + Redis + моковый OpenRouter (`wiremock`).
- Сценарий: «получили медиа → описали → сохранили в SQLite → саммари сработала → событие проскорено и сохранено → ретрив для следующего ответа возвращает его».

**Глазами:**
- После деплоя — 1-2 дня в реальном чате.
- Дашборд: `events_scored_total` по категориям, `vision_describe_total` vs `vision_cache_hit_total`, `decision_skip_total`.
- Раз в день — `SELECT category, count(*) FROM chat_events GROUP BY category` чтобы увидеть распределение.

---

## 12. Бюджет

При активном чате 200 сообщений/день, 30 уникальных картинок/день, 30 ответов/день:

| Компонент | Цена/день | Цена/мес |
|---|---|---|
| Reply (DeepSeek Flash) | ~$0.02 | $0.60 |
| Vision (Gemini 2.5 Flash) | ~$0.002 | $0.06 |
| Embedding (BGE-M3) | копейки | <$0.05 |
| Score-LLM (Nemotron :free) | $0 | $0 |
| Saммари + факты | ~$0.008 | $0.25 |
| **Итого** | **~$0.03** | **~$1** |

Бюджетная подушка: даже при удвоении трафика ≤ $2/мес.

---

## 13. Откат

- Migration `0002` reversible через `DROP TABLE chat_events` + `CREATE TABLE lore (...)` (старая схема в Git-истории).
- Qdrant: удалённую `lore` коллекцию воссоздать `ensure_collections` нельзя (она не в `COLLECTIONS` после реворка); если откат нужен — вернуть строку в `COLLECTIONS`.
- Decision-слой: вернуть `random_p` в конфиг и три строки в `stage1_probability`.

Полный откат к текущему состоянию занимает ~1 commit.

---

## 14. Открытые вопросы

- **Точные ID vision-моделей** — проверить через OpenRouter `/models` непосредственно перед PR (модельный зоопарк меняется).
- **Триггер «бот вклинивается сам»** — пока не нужно (пользователь явно сказал «только когда обращаются»). Если позже захочется — отдельная команда `/wake` или таймер тишины.
- **Видео-кружки** — отдельная фича, требует ffmpeg в рантайм-образе.
