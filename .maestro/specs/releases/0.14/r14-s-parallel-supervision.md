---
title: 'E13.1 — Sweep — compute pool utilisation metrics, significant children and max_children: r14-s-parallel-supervision'
slug: r14-s-parallel-supervision
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'conc.parallel#8 (row conc.parallel 88 → 100). What exists: `AdmissionController::waiting` / `available` / `max_permits` (crates/id_effect/src/compute/admission.rs:66,100,104) and `FiberPool::target_size` (compute/fiber_pool.rs:52); the Rayon pool (compute/rayon_pool.rs:7 `POOL`, :14 `configure_rayon_threads`, :24 `install_parallel`) exposes nothing, and no compute gauge reaches `runtime_metrics::snapshot` (crates/id_effect/src/observability/runtime_metrics.rs:54, which the Prometheus endpoint of id_effect_axum appends). Built: new crates/id_effect/src/compute/utilisation.rs with `PoolUtilisation { workers, active_workers, queue_depth, admitted, admission_waiting }` (`Copy`, documented field by field); `install_parallel` counts calls waiting for the pool (`queue_depth`) and running on it (`active_workers`) with relaxed atomics (no lock added to the install path); `ComputeFabric::utilisation()` (compute/fabric.rs) joins them with the admission counts; `runtime_metrics::snapshot` adds gauges `id_effect_compute_workers`, `id_effect_compute_active_workers`, `id_effect_compute_queue_depth`, `id_effect_compute_admission_waiting` (feature `compute`).'
  - 'fault.supervision#8 (row fault.supervision 88 → 100), in crates/id_effect_process (the supervisor of the scorecard evidence; not crates/id_effect/src/coordination/supervisor.rs). `ChildSpec::significant()` (crates/id_effect_process/src/supervisor.rs:89, new `significant: bool` field) and `SupervisorSpec::auto_shutdown(AutoShutdown::{Never, AnySignificant, AllSignificant})` (supervisor.rs:176, new field, default `Never`): when a significant child exits and is not restarted, the supervisor shuts down its other children by their `ShutdownPolicy` and exits `Shutdown`, as OTP `auto_shutdown`; a significant `Permanent` child is rejected at start (`SupError::SignificantPermanent(id)`, supervisor.rs:253). `DynamicSupervisorSpec::max_children: Option<usize>` (crates/id_effect_process/src/dynamic.rs:25, default `None`): `start_dyn_child` past the limit replies `StartError::MaxChildren(limit)` (crates/id_effect_process/src/msg.rs:175, new variant) without calling the start closure.'
  - 'Semver: `ChildSpec`, `SupervisorSpec` and `DynamicSupervisorSpec` have public fields and `StartError` / `SupError` are exhaustive, so the new fields and variants break struct literals and matches; each break gets its section in docs/MIGRATION-0.14.md (use the builders / `..Default::default()`; add the match arm). The builders keep every 0.13 call compiling.'
  - 'Tests: crates/id_effect/tests/compute_utilisation.rs `idle_pool_reports_zero_active_and_zero_queued`, `utilisation_counts_active_workers_during_install`, `utilisation_counts_installs_waiting_for_the_pool`, `utilisation_reports_admission_waiters`, `runtime_metrics_snapshot_exports_compute_gauges`; crates/id_effect_process/tests/supervision_limits.rs `any_significant_exit_shuts_the_supervisor_down`, `all_significant_waits_for_the_last_significant_child`, `never_auto_shutdown_keeps_running_after_a_significant_exit`, `a_restarted_significant_child_does_not_trigger_shutdown`, `significant_permanent_child_is_rejected_at_start`, `dynamic_supervisor_refuses_children_past_max_children`, `a_terminated_child_frees_a_max_children_slot`; the existing suites crates/id_effect_process/tests/{supervision_contracts,supervision_trees}.rs pass unchanged.'
  - 'Book: crates/id_effect/book/src/part3/ch12-00-compute-fabric.md section "Pool utilisation"; crates/id_effect/book/src/part3/ch09-05-supervision.md section "Significant children, auto_shutdown and max_children". Examples: crates/id_effect/examples/124_compute_pool_metrics.rs (an `[[example]]` entry with `required-features = ["compute"]` in crates/id_effect/Cargo.toml, as 120-122), crates/id_effect_process/examples/131_significant_children.rs.'
  - 'Tier: core (id_effect) and runtime (id_effect_process), both strict bar. Secure by default: no new surface; the gauges are process-wide counts with no labels from input (bounded cardinality), and `max_children` is the R1-style bound a dynamic supervisor lacked (default stays unbounded for compatibility, documented).'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect --all-features --summary-only --fail-under-regions 96`; `cargo llvm-cov nextest -p id_effect_process --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect -p id_effect_process --all-features`; `cargo clippy -p id_effect -p id_effect_process --all-targets --all-features -- -D warnings`; `cargo semver-checks check-release -p id_effect_process --baseline-rev v0.13.0` (every reported break is in docs/MIGRATION-0.14.md)'
  - 'Owns: crates/id_effect/src/compute/utilisation.rs (new), crates/id_effect/src/compute/rayon_pool.rs, crates/id_effect/src/compute/fiber_pool.rs, crates/id_effect/src/compute/fabric.rs (new `utilisation` method only), crates/id_effect/src/compute/mod.rs (module and re-export lines, append-only), crates/id_effect/src/observability/runtime_metrics.rs, crates/id_effect/tests/compute_utilisation.rs (new), crates/id_effect/examples/124_compute_pool_metrics.rs (new), crates/id_effect/Cargo.toml (`[[example]]` entry, append-only), crates/id_effect_process/src/supervisor.rs, crates/id_effect_process/src/dynamic.rs, crates/id_effect_process/src/msg.rs (`StartError` only), crates/id_effect_process/tests/supervision_limits.rs (new), crates/id_effect_process/examples/131_significant_children.rs (new), crates/id_effect/book/src/part3/ch12-00-compute-fabric.md and ch09-05-supervision.md (new sections only), docs/MIGRATION-0.14.md (append-only), crates/id_effect/src/lib.rs (re-export line, append-only), CHANGELOG.md (append-only)'
---

# r14-s-parallel-supervision

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 — Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r14-s-core-runtime (wave 1, same crate id_effect), r14-t0-skeletons (creates the
docs/MIGRATION-0.14.md headings). Blocks: r14-w-warden-core (the Warden supervisor tree uses significant
children and `max_children`, D1), r14-w-warden-mesh (promotes fault.supervision), r14-p-ledger (promotes
conc.parallel), r14-e-benches (pool metrics bench), r14-x-level-audit. Within crates/id_effect/src this
leaf owns only `compute/*` (the four files above) and `observability/runtime_metrics.rs`; no other
wave-2 leaf in this set edits crates/id_effect/src (r14-s-cron-persistent, if it touches
`scheduling/*`, must stay out of `compute/*` and `observability/*`).

Finding at HEAD: `install_parallel` holds the `POOL` mutex guard for the whole `pool.install(f)`
(rayon_pool.rs:29-31), so concurrent installs from different threads run one at a time and
`active_workers` could never exceed one install. The leaf stores the pool as an `Arc<ThreadPool>`,
clones it out under the lock and installs without holding the lock (the test
`utilisation_counts_active_workers_during_install` runs two installs at once).

Defaults the owner may overturn: (1) `queue_depth` counts `install_parallel` calls not yet running on
the pool, admission waiters are reported separately; Rayon's internal job deque is not exposed by
rayon and is not measured. (2) `auto_shutdown` follows OTP: significant children must be `Transient` or
`Temporary`. (3) `max_children` lives on the dynamic supervisor only (OTP has it there only); the
static supervisor's child count is fixed by its spec. Non-goals: per-job metrics, histograms of queue
wait, a `max_children` on `SupervisorHandle::start_child`.
