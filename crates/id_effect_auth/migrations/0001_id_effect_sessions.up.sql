-- Server-side sessions for id_effect_auth's `PgSessionStore` (feature `pg`).
-- `PgSessionStore::ensure_schema` runs these same statements; every one is idempotent.
-- The primary key is the lowercase hex SHA-256 of the session id: the id itself (the cookie
-- value) is never stored, so a read of this table yields no usable session.
CREATE TABLE IF NOT EXISTS id_effect_sessions (
  id_hash text PRIMARY KEY,
  user_id text NOT NULL,
  attrs jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL,
  last_seen_at timestamptz NOT NULL,
  idle_expires_at timestamptz NOT NULL,
  absolute_expires_at timestamptz NOT NULL
);

-- `remove_user` (revoke every session of a user).
CREATE INDEX IF NOT EXISTS id_effect_sessions_user_id_idx
  ON id_effect_sessions (user_id);

-- `purge_expired`.
CREATE INDEX IF NOT EXISTS id_effect_sessions_absolute_expires_at_idx
  ON id_effect_sessions (absolute_expires_at);
