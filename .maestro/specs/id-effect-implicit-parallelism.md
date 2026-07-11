---
title: id_effect implicit parallelism API collapse
slug: id-effect-implicit-parallelism
mode: heavy
work_type: initiative
risk_class: high
version: 1
acceptance_criteria:
  - ADR 0008 documents Fabric-only control plane and supersedes ADR 0006 public Parallelism surface
  - Fabric installed on run() and run_with with run_blocking_serial as sole test escape hatch
  - compute dispatch parallel_if_profitable wires should_parallelize_current and install_parallel across all bulk modules
  - Primary collection and stream chunk APIs use implicit dispatch only with serial escape hatches
  - Stream map dispatches pure Fn versus Effect closure via StreamMapper trait
  - EDG emits independent bind sets with join_binds2 join_binds3 join_binds4
  - Public Parallelism _with _par map_par_n map_par_adaptive removed in 0.4.0
  - Book ch13 ch12 skills CHANGELOG migration for 0.4.0
non_goals:
  - Remote multi-machine execution
  - Arbitrary Effect IR scheduler
  - WASM-specific parallelism backends
---

# id_effect implicit parallelism

Collapse caller-facing parallelism policy into Compute Fabric. See ADR 0008.
