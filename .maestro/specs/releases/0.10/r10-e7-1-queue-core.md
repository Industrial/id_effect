---
title: 'E7.1 — Jobs, `id_effect_jobs`: r10-e7-1-queue-core'
slug: r10-e7-1-queue-core
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - '`Job` trait (Schema-encoded payload, name, queue, priority, max_attempts, unique_key, run_at); `JobQueue` capability with `PgJobQueue` and `MemoryJobQueue`; `enqueue` and `enqueue_in(&SqlTransaction)` (#8); dequeue `FOR UPDATE SKIP LOCKED` ordered by priority then run_at (#4, #5); states queued/running/succeeded/failed/dead, next attempt from a core `Schedule`, `dead` after max_attempts (#2); partial unique index on `(queue, unique_key)` where state is queued/running (#3); `Worker` with per-queue concurrency (#6), heartbeat + visibility timeout re-queues jobs of a crashed worker (#1), drains on shutdown. Tests `skip_locked_two_workers_never_double_run`, `retry_backoff_then_dead`, `unique_key_dedups_while_pending`, `priority_order_within_queue`, `run_at_respected_under_test_clock`, `enqueue_in_rolls_back_with_tx`, `per_queue_concurrency_limit`, `crashed_worker_job_is_requeued`. **API + schema reviewed in a design PR before E7.3/E1.3r start.**'
  - 'Gate: `<pg> -p id_effect_jobs --all-features`'
  - 'Owns: `crates/id_effect_jobs/src/{job.rs,queue.rs,pg.rs,memory.rs,worker.rs}`, `crates/id_effect_jobs/migrations/`'
---

# r10-e7-1-queue-core

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.1 — Jobs, `id_effect_jobs` (L, wave 1 core → wave 2) · msg.jobs #2 #3 #4 #5 #6 #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
