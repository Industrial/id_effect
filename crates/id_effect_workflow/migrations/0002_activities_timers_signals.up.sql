-- Activities, durable timers and signals (id_effect_workflow::migrator, after 0001).
-- Every statement is idempotent. Times are bound by the engine's WallClock, never now().

-- A parked instance (waiting for an activity, a timer or a signal) is not claimable until a
-- completion or a signal clears `waiting`. Each of them also increments `wakeups` under the row
-- lock, so a run that parks only succeeds when nothing arrived since it claimed the instance.
ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS waiting boolean NOT NULL DEFAULT false;
ALTER TABLE workflow_instances ADD COLUMN IF NOT EXISTS wakeups bigint NOT NULL DEFAULT 0;

-- Claim: running instances that are not parked, oldest first.
DROP INDEX IF EXISTS workflow_instances_claim_idx;
CREATE INDEX IF NOT EXISTS workflow_instances_ready_idx
  ON workflow_instances (name, created_at, id) WHERE status = 'running' AND NOT waiting;

-- What workers deliver for one history position: an activity's result or a fired timer. The
-- key is the activity's idempotency key (instance, seq), so a second delivery inserts nothing.
CREATE TABLE IF NOT EXISTS workflow_completions (
  instance_id  text        NOT NULL REFERENCES workflow_instances (id) ON DELETE CASCADE,
  seq          bigint      NOT NULL CHECK (seq >= 1),
  kind         text        NOT NULL,
  name         text        NOT NULL,
  payload      jsonb       NOT NULL,
  completed_at timestamptz NOT NULL,
  CONSTRAINT workflow_completions_pkey PRIMARY KEY (instance_id, seq)
);

-- Signals sent to an instance. `number` is the instance's `wakeups` after the send, taken under
-- the instance row lock, so numbers grow in commit order (arrival order).
CREATE TABLE IF NOT EXISTS workflow_signals (
  instance_id text        NOT NULL REFERENCES workflow_instances (id) ON DELETE CASCADE,
  number      bigint      NOT NULL CHECK (number >= 1),
  name        text        NOT NULL,
  payload     jsonb       NOT NULL,
  received_at timestamptz NOT NULL,
  CONSTRAINT workflow_signals_pkey PRIMARY KEY (instance_id, number)
);

-- The next signal of one name: `wait_signal` reads the first one after the last it consumed.
CREATE INDEX IF NOT EXISTS workflow_signals_name_idx
  ON workflow_signals (instance_id, name, number);
