---
title: 'E7.5 — Workflow engine on Postgres (multi-writer): r11-e7-5-engine'
slug: r11-e7-5-engine
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'Design PR first: the Workflow trait, WorkflowContext and the Postgres schema are reviewed before r11-e7-5-activities-timers starts (the PR link is the first evidence item).'
  - 'Workflow trait (Schema-encoded input and output, name, a version string recorded on each instance); WorkflowContext::step(name, effect) journals each result and on resume returns the journaled result without re-running the step (fault.workflows#2); ctx.now() and ctx.random() are journaled (0.11.0.md D7); a replay whose step name or order differs from the history fails with a typed NonDeterministic error.'
  - 'Postgres schema (migrations/0001): workflow_instances (id, name, status, input, output, version, lease_owner, lease_expires_at, fencing_token) and workflow_history (instance_id, seq, kind, payload, unique (instance_id, seq)); state survives a process restart (#1).'
  - 'Multi-writer (D7): N engines run against one database; an instance advances only under its lease; every history append checks the fencing token, so a stale writer whose lease expired cannot append; an expired lease is taken over and the instance resumes from history on another engine.'
  - 'Built on id_effect_sql SqlClient/SqlTransaction (0.11.0.md D6; no duroxide, no SQLite store); MemoryWorkflowStore for unit tests; start, status and result APIs.'
  - 'Tests `replay_returns_journaled_results_without_rerunning_steps`, `two_engines_never_advance_one_instance_concurrently`, `stale_writer_append_is_rejected_by_fencing`, `crashed_engine_instance_resumes_elsewhere`, `nondeterministic_replay_is_detected`.'
  - 'Gate: `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_workflow --all-features` (Postgres required)'
  - 'Owns: `crates/id_effect_workflow/Cargo.toml`, `crates/id_effect_workflow/src/{lib,workflow,engine,history,lease,memory,pg,error}.rs`, `crates/id_effect_workflow/migrations/0001_*`, `crates/id_effect_workflow/tests/{engine_pg.rs,replay.rs}`'
---

# r11-e7-5-engine

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E7.5 — Workflows". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r11-t0-skeletons (crate skeleton). The pg-integration CI job runs this crate once
r11-sc-pin-permissions adds `-p id_effect_workflow`. Second most likely leaf to be wrong (lease,
fencing and replay); the design PR is the mitigation.
