-- Чаты
CREATE TABLE chats (
  id INTEGER PRIMARY KEY,
  title TEXT,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  is_active INTEGER NOT NULL DEFAULT 1
);

-- Юзеры
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
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
  media_description TEXT,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_messages_chat_created ON messages(chat_id, created_at DESC);
CREATE INDEX idx_messages_user ON messages(user_id, created_at DESC);

-- Эпизодические саммари
CREATE TABLE episodic_summaries (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  chat_id INTEGER NOT NULL REFERENCES chats(id),
  text TEXT NOT NULL,
  qdrant_point_id TEXT NOT NULL,
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
  fact_type TEXT,
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
  tags TEXT,
  qdrant_point_id TEXT NOT NULL,
  added_by INTEGER,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_lore_chat ON lore(chat_id);
