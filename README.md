# Telegram-бот с RAG-памятью

Telegram-бот для группового чата 20–50 человек. Ведёт себя как член чата, помнит контекст и юзеров. Стек: Rust + teloxide, SQLite + Qdrant + Redis, LLM через OpenRouter, эмбеддинги через DeepInfra.

Полная спецификация — в [`CLAUDE.md`](CLAUDE.md). План реализации по шагам — в [`PROMTS.md`](PROMTS.md). Шпаргалка — в [`CHEATSHEET.md`](CHEATSHEET.md).

## Локальный запуск

1. Скопируйте пример окружения:
   ```bash
   cp .env.example .env
   ```
2. Заполните `.env` значениями `TG_BOT_TOKEN`, `OR_API_KEY`, `DEEPINFRA_KEY`.
3. Скопируйте конфиг:
   ```bash
   cp config/config.toml.example config/config.toml
   ```
   и при необходимости подправьте `admin_ids` и параметры памяти.
4. Создайте папку данных:
   ```bash
   mkdir -p data
   ```
5. Запустите контейнеры:
   ```bash
   docker compose up --build
   ```

## Миграции

Миграции прогоняются автоматически при старте бота (через `sqlx::migrate!`). Если нужно прогнать вручную:

```bash
sqlx migrate run --database-url sqlite:./data/bot.db
```

## Healthcheck и метрики

- `curl http://localhost:8080/healthz` — `200 OK`, если SQLite, Qdrant и Redis живы; иначе `503` с описанием.
- `curl http://localhost:9090/metrics` — Prometheus-метрики.

## Полезные команды

Логи бота:
```bash
docker compose logs -f bot
```

Рестарт бота:
```bash
docker compose restart bot
```

Ручной бэкап базы:
```bash
cp data/bot.db data/bot.db.$(date +%F)
```
