-- Security state shared by every instance (0.11 M18, H5): the last TOTP step each user consumed,
-- for id_effect_auth's `PgTotpStepStore` (features `pg` + `totp`), and failure counts for
-- `PgThrottleStore` (feature `pg`; `LoginThrottle` and the TOTP lockout).
-- `PgTotpStepStore::ensure_schema` and `PgThrottleStore::ensure_schema` run these same
-- statements; every one is idempotent.
CREATE TABLE IF NOT EXISTS id_effect_totp_steps (
  user_id text PRIMARY KEY,
  last_step bigint NOT NULL CHECK (last_step >= 0)
);

-- One row per throttle key (`account:<name>`, `ip:<address>`, `totp:<user id>`).
CREATE TABLE IF NOT EXISTS id_effect_auth_failures (
  key text PRIMARY KEY,
  count bigint NOT NULL CHECK (count > 0),
  last_at timestamptz NOT NULL
);

-- `purge_forgotten`.
CREATE INDEX IF NOT EXISTS id_effect_auth_failures_last_at_idx
  ON id_effect_auth_failures (last_at);
