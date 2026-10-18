---
title: 'E7.5 — Activities, durable timers and signals: r11-e7-5-activities-timers'
slug: r11-e7-5-activities-timers
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'fault.workflows#7: activities run as id_effect_jobs jobs (crates/id_effect_jobs/src/job.rs:26 Job, queue.rs:342 JobQueue) on a named queue, retried by a core Schedule, executed at least once by workers on any node, with idempotency key (instance id, seq) so a duplicate completion is ignored; the result is appended to the history.'
  - 'fault.workflows#4: ctx.sleep(d) and ctx.sleep_until(t) create durable timers fired through the job queue run_at (one job per timer, unique key); a timer survives an engine restart; waits of days complete under the test clock.'
  - 'fault.workflows#5: WorkflowClient::signal(instance, name, payload) stores the signal; ctx.wait_signal(name) receives signals in arrival order; a signal sent before the wait is buffered.'
  - 'Tests `activity_retried_until_success_then_journaled`, `activity_runs_at_least_once_after_worker_crash`, `activity_result_dedup_by_idempotency_key`, `durable_timer_survives_engine_restart`, `timer_of_days_under_test_clock`, `signal_before_wait_is_buffered`, `signals_delivered_in_order`.'
  - 'Gate: `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_workflow --all-features` (Postgres required)'
  - 'Owns: `crates/id_effect_workflow/src/{activity,timer,signal,lib}.rs`, `crates/id_effect_workflow/Cargo.toml` (id_effect_jobs dependency), `crates/id_effect_workflow/migrations/0002_*`, `crates/id_effect_workflow/tests/{activities_pg,timers_pg,signals_pg}.rs`'
---

# r11-e7-5-activities-timers

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E7.5 — Workflows". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r11-e7-5-engine and its approved design PR.
