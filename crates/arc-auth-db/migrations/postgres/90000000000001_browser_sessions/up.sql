CREATE TABLE IF NOT EXISTS browser_sessions (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS browser_sessions_user ON browser_sessions(user_id);
CREATE INDEX IF NOT EXISTS browser_sessions_expiry ON browser_sessions(expires_at);
