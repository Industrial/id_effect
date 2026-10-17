---
title: 'RELOAD — config hot reload + rolling restart: r10-reload-config'
slug: r10-reload-config
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Profile files polled by mtime; a change re-parses and publishes through a `SubscriptionRef<C>`; an invalid change keeps the previous value and reports the errors (#5; also the strict reading of sec.config#8, G1). Tests `profile_file_change_notifies_subscribers`, `invalid_change_keeps_previous` (under the test clock).'
  - 'Gate: `cargo nextest run -p id_effect_config --all-features`'
  - 'Owns: `crates/id_effect_config/src/{profile.rs,watch.rs}`'
---

# r10-reload-config

Leaf of `.maestro/specs/releases/0.10.0.md`, section "RELOAD — config hot reload + rolling restart (M, wave 1) · fault.reload #4 #5". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
