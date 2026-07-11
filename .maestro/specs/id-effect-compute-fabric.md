---
title: id_effect Compute Fabric
slug: id-effect-compute-fabric
mode: heavy
work_type: initiative
risk_class: high
version: 1
acceptance_criteria:
  - ADR 0007 compute-fabric documents core principle and ADR 0006 relationship
  - compute module with ResourcePolicy MetricMode WorkProfile TelemetryEngine and ComputeSupervisor skeleton
  - FiberPool work-stealing executor replaces one-thread-per-fiber ThreadSleepRuntime
  - AdmissionController throttles fibers from live telemetry vs ResourcePolicy
  - spawn_scoped_with parent budget inheritance and TokioRuntime ComputeFabric wrapper
  - Effect Dependency Graph auto-parallelizes independent effect binds with safety gates
  - Parallelism and Stream map_par_adaptive use supervisor-driven thresholds and CPU spread mode
  - ClusterResourcePolicy scale_out via jobs workflow with worker Fabric executor
  - Fabric installed by default at run boundaries with book skills examples and ADR 0006 migration
non_goals:
  - WASM-specific Fabric telemetry backends
  - Full arbitrary effect serialization for cluster migration in v1
  - Replacing failure Supervisor with ComputeSupervisor
  - GPU or custom metric policies beyond extensible enum stub
---

# id_effect Compute Fabric

Resource-aware execution substrate: ComputeSupervisor governs CPU/memory against declarative policies, auto-parallelizes independent effect binds, unifies fiber/Rayon/async pools, and extends placement to cluster nodes via jobs/workflow.

## Policy model

- ResourcePolicy: cpu + memory MetricPolicy (extensible)
- MetricMode: Max, Target, Spread, Unlimited
- RebalanceStrategy: ThrottleAdmission, ShedLoad, ScaleOut, ScaleIn
- WorkProfile: CpuIntensive, IoBound, MemoryHeavy, Remote, Mixed

## EDG safety gates

- Data dependency: no read before write completes
- Env borrows: no overlapping mut R across parallel steps
- Opt-out: effect(serial) attribute
- Conservative unknown analysis falls back to sequential

## Cluster scope

ClusterResourcePolicy with global + per_node caps; ScaleOut via id_effect_jobs and id_effect_workflow journal.

See .cursor/plans/compute_fabric_core_a33e415c.plan.md for full architecture.
