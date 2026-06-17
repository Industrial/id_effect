---
title: id_effect v2 Capability DI
slug: id-effect-v2-capability-di
mode: heavy
work_type: initiative
risk_class: high
version: 1
acceptance_criteria:
  - cargo test --workspace passes with zero public v1 DI symbol references outside internal modules
  - id_effect_platform id_effect_logger id_effect_config migrated to Provider and caps
  - examples/040_capability_app.rs demonstrates full capability stack
  - CapabilityGraph reports missing-provider conflicting-provider cycle-detected with trait names
  - mdBook part2 ch04-ch07 rewritten for capability model
  - workspace version 2.0.0 with CHANGELOG migration table
non_goals:
  - Full Effect.ts parity beyond DI
  - Runtime reflection-based injection
  - Changing Effect error algebra or fiber runtime
---

# id_effect v2 Capability DI

Replace Tag/Layer/Cons HList DI with trait-first capabilities, CapabilityGraph auto-resolution, and run/run_with entrypoint. Semver-major clean break v2.0.0.
