---
title: 'E7.1 — Jobs, `id_effect_jobs`: r10-e7-1-cron'
slug: r10-e7-1-cron
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`CronRegistry` entries enqueue a job per tick with `unique_key = name@tick`, so N schedulers enqueue once; leader election by `ClusterSingleton` over `PgAdvisoryLease`. Tests `cron_tick_enqueues_once_across_two_schedulers`, `lease_loss_hands_over_schedule`.'
  - 'Gate: `<pg> -p id_effect_jobs --features cron`'
  - 'Owns: `crates/id_effect_jobs/src/cron.rs` (feature `cron`, dep `id_effect_node/pg`)'
---

# r10-e7-1-cron

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.1 — Jobs, `id_effect_jobs` (L, wave 1 core → wave 2) · msg.jobs #2 #3 #4 #5 #6 #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
