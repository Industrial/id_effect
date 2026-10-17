-- id_effect_sql::audit — the hash-chained audit log (PostgreSQL).
--
-- One chain per tenant (`chain` = the tenant, '' without one). Each row stores
-- hash = sha256(prev_hash || canonical record). `audit_log_head` holds every chain's last
-- (seq, hash): appends lock that row, so concurrent appends to one chain serialise.
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
