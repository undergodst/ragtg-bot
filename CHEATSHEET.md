# Шпаргалка проекта

Краткая выжимка ключевых решений. Для быстрой сверки во время работы.

## Что строим

Telegram-бот для группового чата на 20-50 человек. Ведёт себя как **член чата**, не как ассистент. Помнит контекст, троллит, понимает картинки/голосовухи.

## Решения по стеку

| Что            | Выбор                        | Почему                                            |
| -------------- | ---------------------------- | ------------------------------------------------- |
| Язык           | Rust 1.85, edition 2024      | Свежак, хочешь вайбкодить                         |
| Telegram       | teloxide 0.13+               | Стандарт де-факто на Rust                         |
| БД             | SQLite через sqlx            | Хватит, ноль инфры, файл в bind-маунте            |
| Vector DB      | Qdrant                       | Rust-native, годный API                           |
| Cache          | Redis                        | Working memory, кэш медиа, rate-limit             |
| Embeddings     | DeepInfra BGE-M3             | $0.01/1M, мультиязычные, 1024-dim                 |
| LLM main/pro   | DeepSeek V4 Flash            | $0.14/$0.28, быстрый, по-русски годный            |
| Vision         | Nemotron 3 Nano Omni `:free` | Бесплатно, заточен под perception                 |
| Decision       | DeepSeek V3 `:free`          | Дешёвый классификатор                             |
| Деплой         | Docker Compose               | Bot + Qdrant + Redis. SQLite — файл в bind-маунте |

## Цена под Пидрилу (20 чел чат)

- DeepSeek Flash для основных ответов: ~$0.50/мес
- DeepSeek V3/Nemotron `:free`: $0
- Эмбеддинги BGE-M3: ~$0.05/мес
- **Итого: ~$0.50-1/мес**

## Архитектура памяти

```
Сообщение в чат
    ↓
Сохранить в SQLite (сырое)
    ↓
Push в Redis working window (последние 30)
    ↓
Если медиа → Nemotron → описание → закэшировать по SHA256 → в working window
    ↓
Decision: правила → P(answer) → LLM-классификатор
    ↓ (если отвечаем)
Собрать контекст:
  - System prompt (личность)
  - Few-shot examples (5 случайных)
  - Lore (top-3 по semantic search)
  - User facts (top-5 для каждого юзера в окне)
  - Episodic summaries (top-3 по semantic search)
  - Working window (последние 30)
  - Новое сообщение
    ↓
DeepSeek Flash → ответ
    ↓
Послать в TG, сохранить в SQLite + working window
```

## Фоновые задачи

| Задача             | Частота       | Что делает                                                 |
| ------------------ | ------------- | ---------------------------------------------------------- |
| Episodic summarize | Каждые 5 мин  | Если новых сообщений ≥ 20 — саммари последних 50 в Qdrant  |
| Extract user facts | Каждые 10 мин | Если у юзера ≥ 50 новых сообщений — извлечь факты в Qdrant |
| Backup             | Раз в сутки   | SQLite + Qdrant snapshots в data/backups/                  |

## Декомпозиция работ

14 шагов, каждый — отдельный промпт в Claude Code. Подробности в `PROMPTS.md`.

1. Каркас проекта
2. Подключения + healthcheck
3. Teloxide + сохранение сообщений
4. Working memory в Redis
5. Первый ответ через DeepSeek (sanity check)
6. Vision pipeline
7. Episodic memory
8. Semantic memory (факты)
9. Decision layer
10. Lore + админ-команды
11. Rate limiting
12. Метрики и логи
13. Личность (итеративно!)
14. Бэкапы

## Структура data/

```
data/
├── bot.db            # SQLite основная
├── bot.db-wal        # WAL
├── bot.db-shm        # shared mem
├── qdrant/           # Qdrant storage
│   └── ...
├── redis/            # Redis dump
│   └── dump.rdb
└── backups/          # ротация 30 дней
    ├── bot.YYYY-MM-DD-HHMM.db
    └── qdrant/
```

## Открытые вопросы

- [ ] **Имя бота** — финализировать перед стартом, иначе будет `PROJECT_NAME` плейсхолдер по всему коду
- [ ] **Личность** — определиться с тоном (циничный сторожил / энергичный мудак / дед-всё-видавший / холодный наблюдатель / комбо)
- [ ] **OpenRouter биллинг** — починят со стороны OR, **сегодня их биллинг лежит** для всех
- [ ] **Кому давать админ-права** — собрать список user_ids в admin_ids конфига

## Чек-лист перед первым запуском

- [ ] CLAUDE.md в корне
- [ ] git init
- [ ] TG bot token (от @BotFather) → .env
- [ ] OpenRouter API key → .env (когда починят биллинг)
- [ ] DeepInfra API key → .env
- [ ] mkdir -p data
- [ ] docker compose up --build
- [ ] curl localhost:8080/healthz → 200 OK
- [ ] Добавить бота в тестовую группу
- [ ] Написать сообщение, увидеть в логах что сохранилось
- [ ] Упомянуть @bot, получить ответ

## Полезные команды

```bash
# Логи
docker compose logs -f bot

# Зайти в SQLite
sqlite3 data/bot.db
> .tables
> SELECT count(*) FROM messages;

# Проверить Qdrant
curl localhost:6333/collections

# Проверить Redis
docker compose exec redis redis-cli
> KEYS chat:*
> LRANGE chat:-100123:window 0 -1

# Метрики
curl localhost:9090/metrics | grep llm_

# Бэкап вручную
./scripts/backup.sh

# Прогнать миграции вручную (если sqlx-cli установлен)
sqlx migrate run --database-url sqlite:./data/bot.db
```

## Ссылки

- DeepSeek через OpenRouter: https://openrouter.ai/deepseek/deepseek-v4-flash
- Nemotron 3 Nano Omni: https://openrouter.ai/nvidia/nemotron-3-nano-omni:free
- Qdrant docs: https://qdrant.tech/documentation/
- Teloxide: https://docs.rs/teloxide/latest/teloxide/
- sqlx: https://docs.rs/sqlx/latest/sqlx/
