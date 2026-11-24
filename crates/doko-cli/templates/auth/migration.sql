-- Written by `doko generate auth`: the accounts behind src/controllers/auth.rs.
CREATE TABLE users (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  email TEXT NOT NULL UNIQUE,              -- stored lowercase
  password_hash TEXT NOT NULL,
  password_version INTEGER NOT NULL DEFAULT 0,   -- bumped by a reset, which makes reset links single-use
  email_verified_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
