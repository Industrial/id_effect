-- The hash-chained audit log of id_effect_sql's `audit` module (a copy of its
-- `migrations/audit/0001_audit_log.up.sql`). One chain per tenant; `audit_log_head` holds each
-- chain's last (seq, hash), and appends lock that row, so a chain never forks.
CREATE TABLE IF NOT EXISTS audit_log (
  id BIGSERIAL PRIMARY KEY,
  chain TEXT NOT NULL,
  seq BIGINT NOT NULL CHECK (seq > 0),
  actor TEXT NOT NULL,
  tenant TEXT,
  action TEXT NOT NULL,
  entity TEXT NOT NULL,
  before JSONB,
  after JSONB,
  at TIMESTAMPTZ NOT NULL,
  prev_hash BYTEA NOT NULL,
  hash BYTEA NOT NULL,
  UNIQUE (chain, seq)
);

CREATE INDEX IF NOT EXISTS audit_log_entity_at_idx ON audit_log (entity, at);

CREATE TABLE IF NOT EXISTS audit_log_head (
  chain TEXT PRIMARY KEY,
  seq BIGINT NOT NULL,
  hash BYTEA NOT NULL
);
