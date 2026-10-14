# Changelog

## Unreleased — 0.6.0 consolidation (breaking)

Narrows the workspace to its core purpose and hardens what remains. See
[ADR 0010](docs/adrs/0010-crate-tiers.md) and [docs/QUALITY_BAR.md](docs/QUALITY_BAR.md).
0.5.0 was never published; 0.6.0 is the next crates.io release after 0.4.0.
Upgrading from 0.4 or 0.5: see the [0.6 migration guide](docs/MIGRATION-0.6.md).

### Runtime fixes

- `run`, `run_with`, `ThreadSleepRuntime::default()` and the default `TokioRuntime`
  keep a caller-installed Compute Fabric. They used to replace it. They now call the
  new `ensure_default_fabric()`. `install_default_fabric()` still overwrites, and its
  docs say so.
- Telemetry is lazy and never sleeps. `SysinfoTelemetry` creates its
  `sysinfo::System` on the first snapshot, which returns a neutral CPU reading.
  Later samples reuse the cached reading until enough real time has passed. A cold
  `run_with` takes about 1 ms (was about 400 ms); the first Tokio spawn takes about
  3 ms (was about 200 ms).
- New: `ensure_default_fabric`, `installed_fabric::<E>()`, `is_fabric_installed`,
  `uninstall_fabric`, `SysinfoTelemetry::new` / `is_initialized`,
  `ComputeFabric::telemetry`.
- **Breaking:** `SysinfoTelemetry` is no longer a unit struct. Use
  `SysinfoTelemetry::new()`.

### Streams are pull-based

- `map`, `map_serial`, `filter`, `filter_serial`, `take`, `take_while`, `drop_while`,
  `grouped`, `scan`, `map_effect`, `state_scan`, `transduce_items` /
  `via_transducer`, all windows, and `merge` / `combine_latest` / `keyed_join` now
  emit incrementally and work on infinite sources. Previously each one collected its
  whole input first.
- `from_iterable`, `range`, `unfold` and `unfold_effect` produce lazily.
- **Breaking:** `Stream::from_iterable` requires `I::IntoIter: 'static`.
- New `Stream::from_pull` constructor and `PullFn` alias. `Stream::new` and
  `Stream::from_effect` are documented as single-batch.
- `take(n)` never asks upstream for more than it still needs. `take_while` stops at
  the first false.
- Streams end at the first error: the next poll returns `Ok(None)`. This includes
  `chain`, which used to re-poll the failed side.
- `from_channel` and `from_duplex_queue_channel` read one item per pull.
- Fixed: `merge` dropped the rest of the longer side after 64+ elements.
- Fixed: `Channel::from_stream` applied `flat_map_out` only to the first element of
  each chunk. The rest were buffered unchanged, so `[1,2,3]` through
  `x ↦ [x,-x]` read as `1,2,3,-1`.
- `combine_latest` takes one chunk from each side per pull. It used to drain the
  whole left side first.
- `keyed_join`'s memory grows with the number of distinct keys, as documented.
- Time windows emit once the largest timestamp seen passes the window end.
  - `tumbling_by_time`: an item arriving after its bucket was emitted forms a new
    bucket.
  - `sliding_by_time`: items older than an already-emitted window start are dropped.
- Pulls respect a cancelled `CancellationToken` and yield to the executor while
  upstream keeps returning empty batches.
- `map_effect` runs nothing until the first pull.
- New test helper `id_effect::testing::assert_bounded_memory`.

### Workspace restructure (ADR 0010)

- **Labs:** `id_effect_rpc`, `id_effect_ai`, `id_effect_parse`, `id_effect_lint`,
  `id_effect_workflow`, `id_effect_events`, `id_effect_jobs` and `id_effect_graph` move
  to `labs/`. This is a separate Cargo workspace with `publish = false`. These crates
  are no longer published. They are built and tested by their own CI job, and main CI
  no longer builds rdkafka, OpenSSL, bundled SQLite or apalis.
  - `id_effect_jobs` now uses the stable apalis 1.0 family.
- **Merged:** `id_effect_tower` becomes `id_effect_axum::service` (feature `tower`).
  `id_effect_sql_pg` becomes `id_effect_sql::pg` (feature `pg`). The old crates are
  re-export shims for 0.6 only and will be removed in 0.7.
- `id_effect_fsm` no longer depends on `id_effect_workflow`. The durable bridge
  (`register_fsm`, `step_durable`, `restore_state`, `FsmSnapshot`, `WorkflowFsmError`)
  moves to `id_effect_workflow::fsm` (labs).
- `LogLevel` moves into core (`id_effect::LogLevel`); `id_effect_logger` re-exports
  it, and `id_effect_config` no longer depends on `id_effect_logger`.
- `id_effect_optics::transducer` re-exports core's transducer instead of keeping a
  duplicate. Core gains `transducer::take`.
- **Removed:**
  - `#[derive(Fsm)]` and `id_effect::Fsm` (a stub)
  - the generated `T::parser()` from the schema derive (use `id_effect_parse`'s
    `SchemaBridge::parser_for::<T>()`)
  - the empty `algebra` feature
  - the `id-effect` scaffolder binary and `id_effect_cli::generator` (stubs; the
    `id-effect-diagnose` binary stays)
- **Added:**
  - `#[derive(Schema)]` / `id_effect::Schema`. It now also supports `Option<T>` and
    `Vec<T>` fields.
  - `id_effect_platform` feature `test-util`, which `MemoryOAuthClient` now requires.
- **Renamed (old names deprecated):**
  - `ComputeSupervisor` is now `FabricGovernor`, and `ComputeFabric::supervisor()` is
    now `governor()`
  - `id_effect_platform::process` is now `command`
  - `SchemaParser` is now a deprecated alias of `Schema`
- **Changed:** `Monad::flatten` has a real default implementation and no longer calls
  `unimplemented!`.
- **Publishing:** publish order lives in one place
  (`scripts/validate-crate-packaging.sh --print-order`), and `publish.yml` loops over
  it.

### Slimmer core and the new `id_effect_fp` crate (breaking)

- New maintenance-tier crate **`id_effect_fp`**. It takes the code core does not use
  itself: `algebra`, `collections` (persistent structures), `stm`,
  `kernel::{result, reader, thunk}`, and the unused `foundation` pieces (`unit`,
  `never`, `function`, `product`, `coproduct`, `isomorphism`, `option_`). These paths
  move:
  - `id_effect::{algebra, collections, stm}` → `id_effect_fp::{algebra, collections, stm}`
  - `id_effect::kernel::{result, reader, thunk}` → `id_effect_fp::{result, reader, thunk}`
  - Root re-exports such as `EffectHashMap`, `EffectVector`, `RedBlackTree`, `Trie`,
    `MutableList`, `ChunkBuilder`, `Stm`, `TRef`, `TMap`, `atomically`, `Foldable`,
    `Alternative` and `traverse_vec` → `id_effect_fp::…`
- Placement moves to **`id_effect_node::placement`**: `PlacementMode`,
  `ClusterResourcePolicy`, `NodeCandidate`, `LoadReport`, `place`,
  `pick_placement`, `score_candidate` and `ClusterTelemetry`.
  `FabricGovernor::scale_out_placed` becomes the free function
  `id_effect_node::scale_out_placed`.
- New default-on **`compute`** feature: it gates `compute::*` and parallelism, and
  brings in rayon and sysinfo. With `--no-default-features`, core builds without
  rayon, sysinfo or im, and streams, sorts and runtimes run serially.
- `im` is no longer a dependency. The following now use std collections:
  `SpanRecord::annotations`, `TracingFiberRefs::span_annotations`,
  `Metric::snapshot_frequencies`, `Sink::collect_to_map` (`HashMap`) and
  `TimeBucketMap` (`BTreeMap`).
- tokio stays a required dependency (features `macros`, `rt`, `sync` only), because
  the coordination primitives are built on `tokio::sync`.
- Core no longer has a dev-dependency on `id_effect_config`.

### Hardened runtime crates

- **`id_effect_process`:**
  - A stopped process's mailbox is closed on every exit path. Sending to a dead pid
    now returns `SendError::Rejected`; it used to succeed silently.
  - A panic in a handler, `init` or `terminate` is caught. The process exits with
    `ExitReason::Error("panic: …")` and goes through the normal exit path:
    links and monitors fire, pending calls fail with `CallError::Down`, and the
    supervisor restarts it. Before, it never finalized and callers timed out.
  - New `Addr<DynamicSupervisor>::terminate_dyn_child`.
  - Tests went from 23 to 80.
- **`id_effect_fsm` (breaking):**
  - `Interpreter::step` and `run` take the machine by value and return an `Effect`
    yielding the updated machine. The new state is kept only if the action succeeds.
  - `Saga::run` and `compensate_indices` return `Effect`s.
  - Session types are removed (`SessionSend`, `SessionRecv`, `PingPong`, …).
  - `#[derive(Fsm)]` rejects duplicate edges even when the same event is spelled
    two ways (`Go` and `E::Go`).
  - New: a real `#[derive(Fsm)]` (`#[fsm(event = …, initial = …)]`,
    `#[fsm(on(Ev => Target))]`) with compile-time checks, and a gen_statem-style
    process behaviour (`FsmProcess`, `FsmSpec`, `FsmHandle`). fsm now depends on
    `id_effect_process`.
- **`id_effect_node`** (tests went from 30 to 203; bugs found while testing):
  - Call/spawn replies and monitor `Down` messages were broadcast to every peer, so
    one node's reply could complete another node's request. They now go only to the
    sender (new `Mesh::handle_envelope_from`).
  - Monitoring an unreachable node now fires `NoConnection`. Demonitor works (new
    `Mesh::remote_demonitor`). Pending requests are cleaned up on failure or timeout.
  - A child whose takeover fails is kept as orphaned and retried instead of being
    dropped (`DistributedSupervisor::orphaned` / `retry_orphaned`).
    - Placement tries the preferred node and then the local node.
    - Retries run in the background (`on_membership` now returns
      `Option<RetryHandle>`) and skip the departed node until it rejoins.
  - Monitors that peers hold on our processes are keyed by the peer's name and boot
    incarnation, and are removed when the peer goes down. A restarted node can no
    longer receive another incarnation's `Down`.
  - Monitor sinks run after the lock is released, so a sink can call
    `remote_monitor` or `remote_demonitor` without deadlocking.
  - In-memory transport:
    - `close()` closes both ends, and a dropped listener frees its address.
    - Frames that already arrived are delivered after a close or partition.
    - Connections killed by a partition stay dead after `heal`.
  - QUIC: it used to panic whenever it built a TLS config (two crypto providers were
    linked). It now uses an explicit provider and TLS 1.3 only. The server name is
    configurable (`QuicConfig::with_server_name`, default `"cluster"`).
  - **Breaking:** `PgAdvisoryLease` lock keys are now derived with SHA-256 (they
    used to come from std's hasher, which can change between Rust versions). Do not
    run old and new builds against the same singleton.

### Capability ergonomics

- New `Effect::widen_error::<E>()`: lifts an effect whose error is `()`, `Never` or
  `Infallible` into any error type `E`, with no `map_error(|()| unreachable!())`. On a
  real error type it is a compile error (sealed `kernel::InfallibleError`).
- New `Effect::into_env::<R>()` for `R = ()` effects. `~` binds any `()`-environment
  effect in any environment.
- **Breaking:** `EffectLogger` methods (`info`, `debug`, …) no longer take a type
  parameter. They return `Effect<(), EffectLoggerError>`, so write
  `~logger.info("…")` inside `effect!`, or `.into_env()` elsewhere. An explicit
  turbofish no longer compiles.
- `#[provides(Service, with = || …)]` (or `with = |deps: &Env| …`, or
  `with = Self::build`) defines a provider without a hand-written constructor, so
  `#[allow(clippy::new_ret_no_self)]` is no longer needed.
- `type AppCaps = caps!(A, B);` aliases work in `effect!(|r: &mut AppCaps| …)`.
- Capability diagnostics:
  - `#[track_caller]` on `Env::get`, `try_get`, `get_any`, `Needs::need`.
  - **Breaking:** `try_get` and `get_any` misses return
    `CapabilityError::MissingAt { cap, location }`. Match both shapes with
    `err.missing_capability()`.
  - `CapabilityId::name()` and `Display` show service names.
  - `#[diagnostic::on_unimplemented]` hints on `Needs`, `CapabilityKey`,
    `CapabilitySet`, `FromEnv`, `CapKeys`, `HasCap`, `CapWiden` and `CapBind`.
- `#[capability]` is documented as the deprecated no-op it has been since 0.3. It
  will be removed in 0.7.

### Integrations

- **`id_effect_axum` (breaking):**
  - Effects run on an `EffectExecutor`, which replaces `block_in_place`. It is an
    elastic pool of dedicated threads: each in-flight request gets its own thread with
    a private single-threaded runtime. Handler futures are `Send`, a blocking effect
    stalls only its own request, and current-thread runtimes work.
    - Bounded admission: `max_threads` (default 512) plus `queue_capacity` (default
      1024).
    - `SaturationPolicy::Wait` (default) or `Reject`, which answers 503 with
      `Retry-After`.
    - `DropPolicy::RunToCompletion` (default): an effect finishes even if the client
      disconnects. `DropPolicy::Cancel` is opt-in.
    - Configure it with `EffectExecutor::builder()`. A service can take its own
      executor via `EffectService::with_executor`.
  - `run_with_env` and `run_with_caps` now require `Send + 'static` closures and
    `Send` result and error types.
  - `tokio::spawn` inside an effect runs on an executor worker.
  - New request-scoped capability overrides: `CapOverrideLayer`, `RequestCaps`,
    `execute_with_request_caps`.
- **`id_effect_platform`:** the `auth` module is behind an opt-in `auth` feature.
  reqwest 0.13 only.
- **`id_effect_sql`:** live Postgres tests fail when `ID_EFFECT_REQUIRE_PG=1` and no
  database is available (CI's new `pg-integration` job sets it). Otherwise they print
  `SKIP`; they no longer pass silently.
- **`id_effect_cli`:** new example `010_exit_codes`. `examples/cli-minimal` is the
  template, and `scripts/check-version-drift.sh` (run in CI) catches stale
  `id_effect` version pins.
- **`id_effect_resilience`:**
  - New `retry_with_breaker`: retries on a `Schedule` but fails fast when the breaker
    is open.
  - Clock injection: `CircuitBreaker::make_with_clock`, `RateLimiter::make_with_clock`.
  - Bulkhead's docs now explain that it rejects instead of waiting.
- Every published crate has a README with an example and passes the
  quality-bar check for its tier (`scripts/tier-conformance.sh`).

### Tooling

- MSRV is declared: `rust-version = "1.94"`, checked by a CI `msrv` job.
- Every crate declares its tier in `[package.metadata.id_effect]`.
  `scripts/tier-conformance.sh` checks the quality bar for that tier.
- `scripts/readme-crates.sh` generates the README crate table.

## 0.5.0 — Process Mesh & distributed runtime (never published)

Semver-minor release introducing the **Process Mesh**: a BEAM/OTP-style local
process runtime (`id_effect_process`) and a distribution layer (`id_effect_node`)
for node identity, membership, remote processes, and distributed supervision.
Existing cluster/supervision/jobs/workflow stubs are wired into the mesh behind a
single placement scorer. See [ADR 0009](docs/adrs/0009-process-mesh.md) and
[book Part VII](crates/id_effect/book/src/part7/ch36-00-process-mesh.md).

### Added

- **New crate `id_effect_process`** — typed mailboxes, GenServer-shaped behaviors,
  links, monitors, `trap_exit`, exit-signal propagation, and multi-strategy OTP
  supervision trees, deterministic under `TestClock`.
- **New crate `id_effect_node`** — node identity, cookie-authenticated handshake,
  pluggable transports (InMemory/TCP/QUIC), SWIM membership, remote processes,
  process groups, and distributed supervision.
- **Unified placement API** — `id_effect::place` is the single pure entry point,
  fed by `LoadReport` (mesh telemetry DTO) with a `NodeCandidate` adapter. New
  `PlacementMode::RoundRobin` (deterministic rotation) joins `Affinity`,
  `LocalFirst`, and `Spread`.
- **Placement-aware distribution** — `mesh` gains `report_load` / `load_reports` /
  `remote_spawn_placed`; `DistributedSupervisor` places children via
  `id_effect::place`, honoring `spec.prefer` and excluding downed nodes on takeover.
- **Node-aware jobs** — `FabricJobRunner` honors `JobSpec.target_node` via
  `dequeue_for` / `pending_for`; `ComputeSupervisor::scale_out_placed` targets flow
  through to the runner.
- **Distributed workflow journal** — `RemoteStepJournal` backs `DistributedStepJournal`
  over a `JournalTransport` (`JournalServer` + `LoopbackTransport`) with a wire
  protocol and `WorkflowError::Transport`. `NetworkJournalStub` is retained as a
  test double.
- **`PgAdvisoryLease`** (feature `pg`) — `SingletonLease` backed by Postgres
  session-scoped advisory locks on a dedicated worker connection (CP production
  path). `MemoryLease` retained for tests.
- **`concurrency::RestartIntensity`** — shared sliding-window restart-intensity
  primitive (distinct from `RestartWithLimit`'s total-count cap).
- Examples: `132_distributed_supervisor_failover.rs`, `133_cluster_stream_fanout.rs`.

### Changed

- Capability API uses **service type names** directly: `caps!(Counter)`, `require!(Counter)`, `#[provides(Counter)]`.
- `Cap<T>` replaces generated `*Key` types; `#[capability]` is a no-op (removed from public flow).
- Trait-backed services use type aliases (`HttpClientService`, `ConfigProviderService`, …).
- `PlacementMode::LocalFirst` and `Spread` now diverge — previously they scored
  identically. `LocalFirst` prefers the least-loaded node; `Spread` picks lowest raw load.
- `id_effect_process` OTP supervisor delegates restart-intensity accounting to the
  shared `concurrency::RestartIntensity` primitive (behavior preserved).
- **Workspace versioning** — the root manifest owns `[workspace.package]`
  (version/edition/license/repository); every publishable member inherits via
  `version.workspace = true`, so future bumps propagate atomically.

### Removed

- Public `*Key` types and `define_capability!`.

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.5.0** |
| `id_effect_process` | **0.5.0** (new) |
| `id_effect_node` | **0.5.0** (new) |
| workspace adapters | **0.5.0** |

## 0.4.0 — Implicit parallelism (breaking)

Semver-major release collapsing caller-facing parallelism policy into **Compute Fabric**. See [ADR 0008](docs/adrs/0008-implicit-parallelism.md) and [book ch13-05](crates/id_effect/book/src/part4/ch13-05-parallelism.md).

### Breaking

- **Removed public `Parallelism`** and all `*_with(policy, …)` dispatch on collections, `vec`, `order`, and `Stream`
- **Removed deprecated `*_par`**, `Stream::map_par_n`, `Stream::map_par_adaptive`, and public `compute::effective_threshold`
- Bulk and stream-chunk ops no longer accept caller policy — Fabric's `should_parallelize_current` is the sole threshold

### Added

- `compute::dispatch::parallel_if_profitable` — central Fabric-aware Rayon dispatch
- `Stream::map_effect` — admission-bounded concurrent effect mapping per chunk
- EDG parallel codegen for independent bind sets (`join_binds2` / `join_binds3` / `join_binds4`)
- Default Compute Fabric installation in `run()` and `run_with()`; `ensure_run_context()` at run boundaries
- Book rewrite: [Implicit Parallelism](crates/id_effect/book/src/part4/ch13-05-parallelism.md); Compute Fabric cross-refs in [ch12](crates/id_effect/book/src/part3/ch12-00-compute-fabric.md)
- Example: `122_compute_fabric_effect_parallel.rs` (`Stream::map_effect` under Fabric)

### Changed

- Primary `map`, `filter`, `map_values`, `sort_with`, and related bulk APIs parallelize implicitly when Fabric says it is profitable
- `effect!` may run independent `~` binds concurrently when the EDG finds no data or capability conflict
- `*_serial` and `#[effect(serial)]` remain the explicit escape hatches for `FnMut`, non-`Send`, and ordering

### Migration (from 0.3.x parallel-by-default)

| Old | New |
|-----|-----|
| `Parallelism::…` | removed — trust Fabric |
| `map_with(Parallelism::Serial, f)` | `map_serial(f)` |
| `map_with(Parallelism::ForceParallel, f)` | `map(f)` |
| `*_par(f)` | `map(f)` |
| `Stream::map_par_n(n, f)` | `map_effect(f)` |
| `Stream::map_par_adaptive(f)` | `map_effect(f)` |

Prior [ADR 0006](docs/adrs/0006-parallel-by-default.md) bulk behavior is retained internally; the public policy surface is superseded by [ADR 0008](docs/adrs/0008-implicit-parallelism.md).

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.4.0** |
| workspace adapters | **0.4.0** |

## 0.3.0 — DI maturity (breaking)

Semver-major release completing capability-first DI adoption. See [appendix-b-migration.md](crates/id_effect/book/src/appendix-b-migration.md) and [ADR 0004](docs/adrs/0004-provider-parity-and-cap-subtyping.md).

### Removed

- `CapEnv1…CapEnv6` — use `CapList` / `caps!(K0, K1, …)` only
- `#[capability]` / `*Key` types — use service names with `Cap<T>`
- `require!(env, K)` — use `require!(K)` inside `effect!` or `Needs::<K>::need(env)`
- `ctx!`, `req!`, `service_key!`, `Layer`, `Stack`, `Effect::provide`, `IntoBind` (legacy paths)
- `id_effect_config::ambient` — use `Env::scoped` / `build_env`

### Added

- `CapList` + unbounded `caps!` arity
- `Cap<T>`, `#[derive(ProviderSpecDerive)]`, `#[provides(Service)]`, `#[named("variant")]`
- `CapWiden` for capability-set subtyping
- `ProviderSpec::optional_requires`, `shared()`, `refresh_interval()`, `on_refresh()`
- Fiber/request scoped overrides: `with_override`, `with_fiber_and_override`
- `id-effect-diagnose manifest` with TOML/JSON + `--json`
- Examples: `042_effectful_config_provider`, `043_named_variant_providers`, `id_effect_axum` `020_capability_run_with`
- Expanded trybuild corpus (12 cases) under `tests/ui/`

### Version alignment

| Crate | Version |
|-------|---------|
| `id_effect` | **0.3.0** |
| `id_effect_platform` | **4.0.0** |
| workspace adapters | **0.3.0** |

## Pre-0.3 history — "2.0.0" capability-first DI

Internal milestone numbered "2.0.0" before the workspace settled on 0.x semver. It was
never published under that number; its changes shipped as part of 0.3.0 above.
