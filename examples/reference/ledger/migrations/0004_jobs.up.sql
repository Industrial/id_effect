-- The id_effect_jobs queue (a copy of id_effect_jobs's
-- `migrations/0001_id_effect_jobs.up.sql`, so `id-effect migrate check` covers it): the
-- monthly statements, their fan-out and the mail they send run from this table.
CREATE TABLE IF NOT EXISTS id_effect_jobs (
  id           bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  name         text        NOT NULL,
  queue        text        NOT NULL DEFAULT 'default',
  payload      jsonb       NOT NULL,
  state        text        NOT NULL DEFAULT 'scheduled' CHECK (state IN
                 ('scheduled', 'running', 'succeeded', 'failed', 'dead', 'cancelled')),
  priority     smallint    NOT NULL DEFAULT 0,
  attempts     integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  max_attempts integer     NOT NULL DEFAULT 20 CHECK (max_attempts >= 1),
  unique_key   text,
  unique_scope text        NOT NULL DEFAULT 'pending' CHECK (unique_scope IN ('pending', 'pruned')),
  run_at       timestamptz NOT NULL,
  inserted_at  timestamptz NOT NULL,
  attempted_at timestamptz,
  finished_at  timestamptz,
  locked_by    text,
  locked_until timestamptz,
  last_error   text
);

-- Dequeue: one queue, runnable rows, priority then run_at.
CREATE INDEX IF NOT EXISTS id_effect_jobs_dequeue_idx
  ON id_effect_jobs (queue, priority DESC, run_at, id) WHERE state IN ('scheduled', 'failed');

-- Uniqueness: while pending (failed = awaiting a retry), or until pruned for scope 'pruned'.
CREATE UNIQUE INDEX IF NOT EXISTS id_effect_jobs_unique_idx ON id_effect_jobs (queue, unique_key)
  WHERE unique_key IS NOT NULL
    AND (state IN ('scheduled', 'running', 'failed') OR unique_scope = 'pruned');

-- Rescue of expired leases (locked_until is the lease a heartbeat pushes forward).
CREATE INDEX IF NOT EXISTS id_effect_jobs_lease_idx
  ON id_effect_jobs (locked_until) WHERE state = 'running';

-- Pruning finished rows.
CREATE INDEX IF NOT EXISTS id_effect_jobs_finished_idx ON id_effect_jobs (finished_at)
  WHERE state IN ('succeeded', 'dead', 'cancelled');
