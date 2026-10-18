-- Single-use account tokens (email verification, password reset) for id_effect_auth's
-- `PgAccountTokenStore` (feature `accounts`).
-- `PgAccountTokenStore::ensure_schema` runs these same statements; every one is idempotent.
-- A token reads `<id>_<secret>`: `id` is its public lookup key, and only the lowercase hex
-- SHA-256 of the secret is stored, so a read of this table yields no usable token.
CREATE TABLE IF NOT EXISTS id_effect_account_tokens (
  id text PRIMARY KEY,
  kind text NOT NULL CHECK (kind IN ('verify_email', 'reset_password')),
  account_id text NOT NULL,
  secret_hash text NOT NULL,
  expires_at timestamptz NOT NULL
);

-- `remove_account` (a new token of a kind replaces the account's earlier ones).
CREATE INDEX IF NOT EXISTS id_effect_account_tokens_account_idx
  ON id_effect_account_tokens (account_id, kind);

-- `purge_expired`.
CREATE INDEX IF NOT EXISTS id_effect_account_tokens_expires_at_idx
  ON id_effect_account_tokens (expires_at);
