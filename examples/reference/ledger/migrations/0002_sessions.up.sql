-- Server-side sessions for `PgSessionStore` (a copy of id_effect_auth's
-- `migrations/0001_id_effect_sessions.up.sql`, so `id-effect migrate check` covers it). The
-- key is the SHA-256 of the session id; the id itself (the cookie value) is never stored.
CREATE TABLE IF NOT EXISTS id_effect_sessions (
  id_hash text PRIMARY KEY,
  user_id text NOT NULL,
  attrs jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL,
  last_seen_at timestamptz NOT NULL,
  idle_expires_at timestamptz NOT NULL,
  absolute_expires_at timestamptz NOT NULL
);

CREATE INDEX IF NOT EXISTS id_effect_sessions_user_id_idx
  ON id_effect_sessions (user_id);

CREATE INDEX IF NOT EXISTS id_effect_sessions_absolute_expires_at_idx
  ON id_effect_sessions (absolute_expires_at);
