DROP TABLE IF EXISTS workflow_signals;
DROP TABLE IF EXISTS workflow_completions;
DROP INDEX IF EXISTS workflow_instances_ready_idx;
CREATE INDEX IF NOT EXISTS workflow_instances_claim_idx
  ON workflow_instances (name, created_at, id) WHERE status = 'running';
ALTER TABLE workflow_instances DROP COLUMN IF EXISTS wakeups;
ALTER TABLE workflow_instances DROP COLUMN IF EXISTS waiting;
