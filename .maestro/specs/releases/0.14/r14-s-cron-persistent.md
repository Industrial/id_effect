---
title: 'E13.1 Sweep — persistent, runtime-mutable cron schedules in id_effect_jobs (Postgres): r14-s-cron-persistent'
slug: r14-s-cron-persistent
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'msg.cron#6: schedules are rows, added, changed, paused and removed at runtime without a redeploy. Today the schedule set is fixed when the process builds its `CronRegistry` (crates/id_effect/src/scheduling/cron_job.rs:339, `with` :374) and `CronLeader::run` (crates/id_effect_jobs/src/cron.rs:156) runs that registry until the lease is lost. New in crates/id_effect_jobs/src/schedules.rs (feature `cron`): `StoredSchedule` (name, cron expression, time zone, job name, queue, payload `serde_json::Value`, enabled, version), the `ScheduleStore` trait (`upsert`, `remove`, `set_enabled`, `list`, `version`), `PgScheduleStore` on any `SqlClient` (table `id_effect_cron_schedules`, migration `0002_id_effect_cron_schedules.{up,down}.sql` added to `migrator()` at crates/id_effect_jobs/src/pg.rs:113 and to `PgJobQueue::ensure_schema` :302) and `MemoryScheduleStore`; `RuntimeCron` builds a `CronRegistry` from the enabled rows, each tick enqueuing a raw `NewJob` (crates/id_effect_jobs/src/queue.rs:91) through `enqueue_raw` with `tick_key` (cron.rs:33) as its unique key; `CronLeader::run_runtime(&RuntimeCron, clock)` reloads when the store version changes (polled every `reload_every`, default 5 s) by interrupting and restarting the registry run, so a change takes effect within one poll on the current leader and on any later leader. Row msg.cron 88 → 100.'
  - 'Validation on `upsert`: the cron expression parses (`Cron::parse`, crates/id_effect/src/scheduling/cron.rs:143) and the zone resolves, else `ScheduleError::Invalid`; the job name must be in the `RuntimeCron` allow-list of registered job names (`RuntimeCron::allow::<J: Job>()`), so a row can never enqueue a job no worker registered.'
  - 'Tests: crates/id_effect_jobs/tests/cron_schedules_pg.rs (Postgres as crates/id_effect_jobs/tests/cron_pg.rs does, CI job `pg-integration` with `id_effect_jobs/cron` already on, ci.yml:319) `schedule_added_at_runtime_fires_without_restart`, `schedule_removed_at_runtime_stops_firing`, `paused_schedule_does_not_fire_and_resumes`, `changed_cron_expression_applies_on_next_reload`, `runtime_schedule_enqueues_once_across_two_leaders`, `runtime_schedules_survive_leader_handover`; crates/id_effect_jobs/tests/cron_schedules.rs (memory store, virtual clock) `memory_schedule_store_round_trips`, `upsert_rejects_invalid_cron_or_zone`, `schedule_for_unregistered_job_is_rejected`, `schedule_count_is_capped`.'
  - 'Book: new section "Schedules at runtime" after "## Cron" in `crates/id_effect/book/src/part6/ch31-01-jobs.md` (:143); example `crates/id_effect_jobs/examples/020_runtime_schedules.rs` (required-features `cron`).'
  - 'Tier: platform (id_effect_jobs; Ledger consumer). Secure by default (ADR 0013): R4 the store holds at most `max_schedules` rows (default 1 000) and a payload of at most 64 KiB per row, checked on `upsert`; R1 the job-name allow-list above (deny by default); R5 statements are the crate''s own parameterised SQL, no caller SQL. No HTTP surface is added (an admin/console UI for schedules is not in this leaf).'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_jobs --all-features --summary-only --fail-under-regions 96` (with `ID_EFFECT_REQUIRE_PG=1`).'
  - 'Gate: `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_jobs --all-features`; `cargo clippy -p id_effect_jobs --all-targets --all-features -- -D warnings`; `bash scripts/tier-conformance.sh id_effect_jobs`'
  - 'Owns: `crates/id_effect_jobs/src/schedules.rs` (new), `crates/id_effect_jobs/src/cron.rs` (`CronLeader::run_runtime` and its helpers only), `crates/id_effect_jobs/src/pg.rs` (`migrator()`, the schema constants and `ensure_schema` only), `crates/id_effect_jobs/migrations/0002_id_effect_cron_schedules.{up,down}.sql` (new), `crates/id_effect_jobs/tests/{cron_schedules.rs,cron_schedules_pg.rs}` (new), `crates/id_effect_jobs/examples/020_runtime_schedules.rs` (new), `crates/id_effect_jobs/Cargo.toml` (`[[example]]` entry), `crates/id_effect/book/src/part6/ch31-01-jobs.md` (the new section); append-only: `crates/id_effect_jobs/src/lib.rs` (mod and `pub use` lines), `CHANGELOG.md`'
---

# r14-s-cron-persistent

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: the wave-1 merge (no wave-1 leaf owns its files). Blocks nothing in later waves
except the level audit and re-score; Warden's jobs/cron intake (r14-w-warden-core) may use it.

Defaults the owner may overturn: polling the store version (not LISTEN/NOTIFY, which needs a dedicated
connection); restart-on-change of the whole registry (the `MissedRunPolicy` of crates/id_effect/src/
scheduling/cron_job.rs:87 governs ticks missed during the restart; ticks before the reload are not
replayed); stored schedules enqueue jobs only (they do not run arbitrary effects); the core
`CronRegistry` stays immutable (no change to crates/id_effect). Not done here: a console or admin UI to
edit schedules, schedule history in Postgres (history stays in `CronScheduler::history`).
