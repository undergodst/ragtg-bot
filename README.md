<<<<<<< HEAD
# Telegram-бот с RAG-памятью

## Локальный запуск

1. Скопируйте пример окружения:
   ```bash
   cp .env.example .env
   ```
2. Заполните `.env` значениями `TG_BOT_TOKEN`, `OR_API_KEY`, `DEEPINFRA_KEY`.
3. Создайте папку данных:
   ```bash
   mkdir -p data
   ```
4. Запустите контейнеры:
   ```bash
   docker compose up --build
   ```

## Миграции

Вариант 1: вручную через sqlx CLI:
```bash
sqlx migrate run --database-url sqlite:./data/bot.db
```

Вариант 2: запускать миграции на старте бота (будет реализовано на следующих шагах).

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
=======
# ragtg-bot
>>>>>>> 3887370ce65755223e7314ad4e372ceb680c8ca1
