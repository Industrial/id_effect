-- Failure counts for `PgThrottleStore`: the login throttle and the TOTP lockout, shared by every
-- instance (0.11 H5, H6; the `id_effect_auth_failures` part of id_effect_auth's
-- `migrations/0003_id_effect_auth_state.up.sql`, so `id-effect migrate check` covers it). The
-- Ledger keeps the last consumed TOTP step on `users` instead of `id_effect_totp_steps`.
-- One row per throttle key (`account:<name>`, `ip:<address>`, `totp:<user id>`).
CREATE TABLE IF NOT EXISTS id_effect_auth_failures (
  key text PRIMARY KEY,
  count bigint NOT NULL CHECK (count > 0),
  last_at timestamptz NOT NULL
);

-- `purge_forgotten`.
CREATE INDEX IF NOT EXISTS id_effect_auth_failures_last_at_idx
  ON id_effect_auth_failures (last_at);
