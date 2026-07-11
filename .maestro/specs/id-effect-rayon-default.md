---
title: id_effect Rayon parallel-by-default
slug: id-effect-rayon-default
mode: heavy
work_type: initiative
risk_class: medium
version: 1
acceptance_criteria:
  - "Parallelism policy type with Auto threshold 1024 ForceParallel Serial"
  - "Primary map filter map_values sort_with use parallel dispatch by default"
  - "Explicit serial escape hatches preserve FnMut and non-Send types"
  - "par suffix methods deprecated as ForceParallel aliases"
  - "Stream map_par_n unchanged"
  - "effect and effect macro remain sequential"
  - "Book SKILL example 071 and CHANGELOG updated"
  - "Workspace tests clippy mdbook pass"
non_goals:
  - Parallel effect binds in effect macro (deferred to id-effect-implicit-parallelism; see ADR 0008)
  - WASM serial-only crate feature
  - Changing map_par_n semantics (deferred to id-effect-implicit-parallelism; unified Stream::map in 0.4.0)
  - Removing public Parallelism surface (deferred to id-effect-implicit-parallelism; see ADR 0008)
---

# id_effect Rayon parallel-by-default

Flip bulk pure transforms to parallel-by-default via `Parallelism` policy. See ADR 0006.

**Superseded (public API):** ADR 0008 and [id-effect-implicit-parallelism](id-effect-implicit-parallelism.md) collapse caller-facing `Parallelism` / `*_with` / `*_par` into Fabric-only implicit dispatch. Bulk parallel-by-default behavior is retained; EDG parallel binds and Stream map unification move to that initiative.
