-- Idempotency keys of the RPC mutations (`src/idempotency.rs`, ADR 0014 D14): a retried
-- `Ledger.post_entry` with the same `Idempotency-Key` gets the first result back on every
-- instance. One row per (caller, procedure, key); `body` is null while the first call runs.
CREATE TABLE rpc_idempotency (
  scope text NOT NULL,
  tag text NOT NULL,
  key text NOT NULL,
  -- SHA-256 (hex) of the procedure and its payload: a reused key with another payload is a 409.
  digest text NOT NULL,
  status bigint,
  body text,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (scope, tag, key)
);

-- Expired keys (older than a day) are deleted per scope as keys are reserved.
CREATE INDEX rpc_idempotency_created_at_idx ON rpc_idempotency (scope, created_at);
