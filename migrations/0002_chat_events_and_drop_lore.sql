-- chat_events: автоматически наполняемая память значимых моментов чата.
-- Заменяет ручной лор (table `lore`).
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

-- Лор был ручной (/lore_add), на проде пуст. Сносим.
DROP TABLE IF EXISTS lore;
