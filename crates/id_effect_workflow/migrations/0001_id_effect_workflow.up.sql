-- The id_effect_workflow tables (id_effect_workflow::migrator, PgWorkflowStore::ensure_schema).
-- Every statement is idempotent. Times are bound by the engine's WallClock, never now().
CREATE TABLE IF NOT EXISTS workflow_instances (
  id               text        PRIMARY KEY,
  name             text        NOT NULL,
  version          text        NOT NULL,
  status           text        NOT NULL DEFAULT 'running' CHECK (status IN
                     ('running', 'completed', 'failed')),
  input            jsonb       NOT NULL,
  output           jsonb,
  error            text,
  lease_owner      text,
  lease_expires_at timestamptz,
  fencing_token    bigint      NOT NULL DEFAULT 0 CHECK (fencing_token >= 0),
  created_at       timestamptz NOT NULL,
  updated_at       timestamptz NOT NULL
);

-- Claim: running instances of one workflow, oldest first (a free or expired lease is checked
-- on the rows the index returns).
CREATE INDEX IF NOT EXISTS workflow_instances_claim_idx
  ON workflow_instances (name, created_at, id) WHERE status = 'running';

-- The journal: one row per position, so two writers can never record the same seq.
CREATE TABLE IF NOT EXISTS workflow_history (
  instance_id text        NOT NULL REFERENCES workflow_instances (id) ON DELETE CASCADE,
  seq         bigint      NOT NULL CHECK (seq >= 1),
  kind        text        NOT NULL,
  name        text        NOT NULL,
  payload     jsonb       NOT NULL,
  recorded_at timestamptz NOT NULL,
  CONSTRAINT workflow_history_instance_seq_key UNIQUE (instance_id, seq)
);
