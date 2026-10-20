-- The id_effect_workflow tables (a copy of id_effect_workflow's `migrations/0001_*.up.sql` and
-- `0002_*.up.sql` in their final form, so `id-effect migrate check` covers them): the account
-- closures (`src/closure.rs`) are journaled here. Times are bound by the engine's WallClock,
-- never now(). Activities and timers are jobs of `id_effect_jobs` (`0004_jobs`).
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
  updated_at       timestamptz NOT NULL,
  -- Parked: waiting for an activity, a timer or a signal; not claimable until one arrives.
  waiting          boolean     NOT NULL DEFAULT false,
  -- Completions and signals so far: a run parks only when none arrived since its claim.
  wakeups          bigint      NOT NULL DEFAULT 0
);

-- Claim: running instances that are not parked, oldest first.
CREATE INDEX IF NOT EXISTS workflow_instances_ready_idx
  ON workflow_instances (name, created_at, id) WHERE status = 'running' AND NOT waiting;

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

-- What workers deliver for one history position: an activity's result or a fired timer, once
-- per idempotency key (instance, seq).
CREATE TABLE IF NOT EXISTS workflow_completions (
  instance_id  text        NOT NULL REFERENCES workflow_instances (id) ON DELETE CASCADE,
  seq          bigint      NOT NULL CHECK (seq >= 1),
  kind         text        NOT NULL,
  name         text        NOT NULL,
  payload      jsonb       NOT NULL,
  completed_at timestamptz NOT NULL,
  CONSTRAINT workflow_completions_pkey PRIMARY KEY (instance_id, seq)
);

-- Signals sent to an instance (the closure's approval), numbered in arrival order.
CREATE TABLE IF NOT EXISTS workflow_signals (
  instance_id text        NOT NULL REFERENCES workflow_instances (id) ON DELETE CASCADE,
  number      bigint      NOT NULL CHECK (number >= 1),
  name        text        NOT NULL,
  payload     jsonb       NOT NULL,
  received_at timestamptz NOT NULL,
  CONSTRAINT workflow_signals_pkey PRIMARY KEY (instance_id, number)
);

CREATE INDEX IF NOT EXISTS workflow_signals_name_idx
  ON workflow_signals (instance_id, name, number);
