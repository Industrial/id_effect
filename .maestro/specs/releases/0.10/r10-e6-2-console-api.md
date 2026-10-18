---
title: 'E6.2 — Console, `id_effect_console`: r10-e6-2-console-api'
slug: r10-e6-2-console-api
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`Console::router(guard: Guard)` — no unguarded constructor; JSON under `/_console/api/`: `fibers` (#1), `supervisors` from `which_children` + `TreeNodeView` (#2), `metrics`, `health`, `log-level` GET/PUT changing the `EnvFilter` through a reload layer and the `EffectLogger` minimum level (#6). Tests `unauthorised_is_403`, `fiber_dump_lists_running_fiber`, `supervisor_tree_matches_which_children`, `log_level_change_takes_effect_without_restart`.'
  - 'Gate: `cargo nextest run -p id_effect_console -p id_effect_opentelemetry --all-features`'
  - 'Owns: `crates/id_effect_console/src/{lib.rs,api.rs,supervisors.rs,log_level.rs}`, `crates/id_effect_opentelemetry/src/subscriber.rs` (reload handle)'
---

# r10-e6-2-console-api

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E6.2 — Console, `id_effect_console` (L, wave 1 registry → wave 2) · obs.console #1 #2 #3 #6". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
