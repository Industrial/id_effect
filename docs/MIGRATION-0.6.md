# Migrating to id_effect 0.6

0.6.0 is a consolidation release (see [ADR 0010](adrs/0010-crate-tiers.md)). It narrows the
workspace to a typed effect runtime plus supervision and distribution, moves unused or
experimental crates out of the published set, and fixes runtime defects that real users hit.
The authoritative list of changes is the `Unreleased — 0.6.0` section of
[CHANGELOG.md](../CHANGELOG.md). This guide reorganises it as "if you used X, now do Y".

Who this is for:

- **crates.io 0.4.0 users.** 0.5.0 was never published, so you get the 0.5 changes (Process
  Mesh, placement API) and the 0.6 changes in one step. The 0.5 additions are new API, not
  breaking changes. Everything below applies to you.
- **path-dependency 0.5.0 users** (a git checkout of the 0.5 tree). Everything below applies.

Contents:

1. [Update the dependency](#1-update-the-dependency)
2. [Crates that moved to `labs/`](#2-crates-that-moved-to-labs)
3. [Merged crates](#3-merged-crates)
4. [Moved paths: `id_effect_fp` and `id_effect_node::placement`](#4-moved-paths)
5. [Renames](#5-renames)
6. [Streams are pull-based](#6-streams-are-pull-based)
7. [Compute Fabric: install-if-absent and lazy telemetry](#7-compute-fabric)
8. [`id_effect_axum`: the effect executor](#8-id_effect_axum-the-effect-executor)
9. [`id_effect_fsm`](#9-id_effect_fsm)
10. [`id_effect_process`](#10-id_effect_process)
11. [`id_effect_node`: singleton lock keys (rolling upgrades)](#11-id_effect_node-singleton-lock-keys)
12. [Other integration crates](#12-other-integration-crates)
13. [Removed stubs and features](#13-removed-stubs-and-features)
14. [Capability ergonomics](#14-capability-ergonomics)
15. [Checklist](#15-checklist)

---

## 1. Update the dependency

Every published crate moves to `0.6.0` in lockstep. Bump all `id_effect*` entries together:

```toml
[dependencies]
id_effect = "0.6"
id_effect_tokio = "0.6"
id_effect_process = "0.6"
# …every other id_effect_* crate you use, also "0.6"
```

The MSRV is now declared: `rust-version = "1.94"`.

Published crates in 0.6, by tier:

| Tier | Crates |
|------|--------|
| core | `id_effect`, `id_effect_macro`, `id_effect_proc_macro`, `id_effect_tokio`, `id_effect_config`, `id_effect_logger`, `id_effect_resilience` |
| runtime | `id_effect_process`, `id_effect_node`, `id_effect_fsm` |
| integration | `id_effect_platform`, `id_effect_axum`, `id_effect_sql`, `id_effect_opentelemetry`, `id_effect_cli` |
| maintenance (frozen API) | `id_effect_fp` (new), `id_effect_optics` |
| shims, 0.6 only | `id_effect_tower`, `id_effect_sql_pg` |

## 2. Crates that moved to `labs/`

These crates are **no longer published**. Their code lives under `labs/`, a separate Cargo
workspace with `publish = false`. They still build and have their own CI job, but they have no
API guarantees and no crates.io releases:

`id_effect_rpc`, `id_effect_ai`, `id_effect_parse`, `id_effect_lint`, `id_effect_workflow`,
`id_effect_events`, `id_effect_jobs`, `id_effect_graph`.

**If you used one of them**, depend on it from git (or from a local checkout) instead of
crates.io. Labs crates depend on the published crates by path, so take the published crates from
the same revision:

```toml
[dependencies]
# Before (0.4 / 0.5)
# id_effect_workflow = "0.4"

# After: git dependency, pinned to a revision
id_effect_workflow = { git = "https://github.com/Industrial/id_effect", rev = "<sha>" }
id_effect = { git = "https://github.com/Industrial/id_effect", rev = "<sha>" }

# Or, with a local checkout
# id_effect_workflow = { path = "../id_effect/labs/id_effect_workflow" }
```

Inside the repository, build or run them through the labs workspace:

```bash
cargo test --manifest-path labs/Cargo.toml -p id_effect_workflow
cargo run --manifest-path labs/Cargo.toml -p id_effect_rpc --example 020_rpc_tracing
```

A labs crate returns to crates.io only when a real consumer depends on it and it passes the quality
bar for its target tier (ADR 0010, rule 2). If you depend on one, say so in an issue.

Related moves:

- `id_effect_jobs` now uses the stable apalis 1.0 family.
- `id_effect_fsm` no longer depends on `id_effect_workflow`. The durable bridge (`register_fsm`,
  `step_durable`, `restore_state`, `FsmSnapshot`, `WorkflowFsmError`) moved to
  `id_effect_workflow::fsm` (labs).
- `SchemaBridge::parser_for::<T>()` (in `id_effect_parse`, labs) replaces the generated
  `T::parser()` (see [section 13](#13-removed-stubs-and-features)).

## 3. Merged crates

Two crates were folded into their natural homes. The old crates still exist for 0.6 as
re-export shims, and **will be removed in 0.7**.

| If you used | Now use | Feature |
|-------------|---------|---------|
| `id_effect_tower::*` | `id_effect_axum::service::*` | `id_effect_axum/tower` |
| `id_effect_sql_pg::*` | `id_effect_sql::pg::*` | `id_effect_sql/pg` |

```toml
# Before
id_effect_tower = "0.4"
id_effect_sql_pg = "0.4"

# After
id_effect_axum = { version = "0.6", features = ["tower"] }
id_effect_sql = { version = "0.6", features = ["pg"] }
```

```rust
// Before
use id_effect_tower::EffectService;
use id_effect_sql_pg::{PgPool, PgSqlClient};

// After
use id_effect_axum::service::EffectService;
use id_effect_sql::pg::{PgPool, PgSqlClient};
```

Only the paths change; the items are the same.

## 4. Moved paths

### Functional-programming modules → `id_effect_fp`

Core no longer carries the code it does not use itself. The new maintenance-tier crate
**`id_effect_fp`** holds it. Add the dependency and change the paths:

```toml
id_effect_fp = "0.6"
```

| If you used | Now use |
|-------------|---------|
| `id_effect::algebra::*` | `id_effect_fp::algebra::*` |
| `id_effect::collections::*` | `id_effect_fp::collections::*` |
| `id_effect::stm::*` | `id_effect_fp::stm::*` |
| `id_effect::kernel::{result, reader, thunk}` | `id_effect_fp::{result, reader, thunk}` |
| `id_effect::foundation::{unit, never, function, product, coproduct, isomorphism, option_}` | `id_effect_fp::foundation::…` |
| root re-exports `EffectHashMap`, `EffectVector`, `RedBlackTree`, `Trie`, `MutableList`, `ChunkBuilder`, `Stm`, `TRef`, `TMap`, `atomically`, `Foldable`, `Alternative`, `traverse_vec`, … | the same names from `id_effect_fp` |

```rust
// Before
use id_effect::{TRef, atomically, EffectHashMap};

// After
use id_effect_fp::{TRef, atomically, EffectHashMap};
```

The `foundation` pieces core uses everywhere (`either`, `func`, `piping`, `predicate`,
`mutable_ref`) stay in `id_effect`.

### Placement → `id_effect_node::placement`

Cluster placement was never used by core and now lives with the distribution layer:

| If you used | Now use |
|-------------|---------|
| `id_effect::{place, pick_placement, score_candidate}` | `id_effect_node::placement::{place, pick_placement, score_candidate}` |
| `id_effect::{PlacementMode, ClusterResourcePolicy, NodeCandidate, LoadReport, ClusterTelemetry}` | `id_effect_node::placement::…` |
| `ComputeSupervisor::scale_out_placed(…)` (method) | `id_effect_node::scale_out_placed(&governor, …)` (free function) |

```rust
// Before (0.5)
use id_effect::{place, ClusterResourcePolicy, LoadReport};

// After
use id_effect_node::placement::{place, ClusterResourcePolicy, LoadReport};
```

### `LogLevel` → core

`LogLevel` is now `id_effect::LogLevel`. `id_effect_logger` re-exports it, so existing
`id_effect_logger::LogLevel` imports keep working.

## 5. Renames

The old names are deprecated aliases for 0.6, so code keeps compiling with warnings.

| Old | New |
|-----|-----|
| `ComputeSupervisor` | `FabricGovernor` |
| `ComputeFabric::supervisor()` | `ComputeFabric::governor()` |
| `id_effect_platform::process` | `id_effect_platform::command` |
| `#[derive(SchemaParser)]` / `id_effect::SchemaParser` | `#[derive(Schema)]` / `id_effect::Schema` |

```rust
// Before
use id_effect::ComputeSupervisor;
use id_effect_platform::process::{CommandSpec, spawn_wait};
#[derive(id_effect::SchemaParser)]
struct User { name: String }

// After
use id_effect::FabricGovernor;
use id_effect_platform::command::{CommandSpec, spawn_wait};
#[derive(id_effect::Schema)]
struct User { name: String }
```

Derive macros cannot carry `#[deprecated]`, so `#[derive(SchemaParser)]` gives **no warning**.
Search for it. `#[derive(Schema)]` also supports `Option<T>` and `Vec<T>` fields now.

`FabricGovernor` was renamed so it is not mistaken for a supervisor: it governs resources and never
restarts work. Supervision is `id_effect::concurrency::Supervisor` (one effect or fiber),
`id_effect_process` (OTP trees) and `id_effect_node::DistributedSupervisor` (across nodes).

## 6. Streams are pull-based

`Stream` now pulls bounded chunks from upstream. Previously most operators collected their whole
input first. This mostly fixes behaviour (infinite sources work, memory is bounded) but has
observable effects:

- **Operators emit incrementally.** This applies to `map`, `filter`, `take`, `take_while`,
  `drop_while`, `grouped`, `scan`, `map_effect`, `state_scan`, transducers, all windows, `merge`,
  `combine_latest` and `keyed_join`. `Stream::from_iterable`, `range`, `unfold` and
  `unfold_effect` produce lazily. Side effects in a source now interleave with downstream
  processing instead of all running first.
- **`take(n)` never asks upstream for more than it still needs.** `take_while` stops at the first
  `false`. Don't count on a source being drained to the end.
- **Errors end the stream.** After a stream yields an error, the next poll returns `Ok(None)`.
  This includes `chain`, which used to poll the failed side again. Don't poll past an error to get
  more items.
- **`map_effect` runs nothing until the first pull.**
- **Breaking:** `Stream::from_iterable` requires `I::IntoIter: 'static`. Pass owned data (a `Vec`,
  a range, `into_iter()` of an owned collection) rather than an iterator that borrows:

  ```rust
  // Before: compiled when the borrow happened to outlive the stream's use
  let s = Stream::from_iterable(items.iter().cloned());

  // After: give the stream an owned iterator
  let s = Stream::from_iterable(items.clone());
  ```

- `Stream::new` and `Stream::from_effect` are single-batch by design. Use the new
  `Stream::from_pull` (`PullFn`) for a custom incremental source.
- Time windows emit once the largest timestamp seen passes the window end. A late item in
  `tumbling_by_time` starts a new bucket. In `sliding_by_time`, items older than an emitted window
  start are dropped.
- New test helper: `id_effect::testing::assert_bounded_memory`. It proves a pipeline does not
  buffer its upstream.

## 7. Compute Fabric

- **Install-if-absent.** `run`, `run_with`, `ThreadSleepRuntime::default()` and the default
  `TokioRuntime` used to *replace* any fabric you had installed. They now call
  `ensure_default_fabric()` and keep yours. If you worked around this by installing again after
  every runtime construction, delete the workaround. `install_default_fabric()` still overwrites,
  on purpose.
- New helpers: `ensure_default_fabric`, `installed_fabric::<E>()`, `is_fabric_installed`,
  `uninstall_fabric`, `ComputeFabric::telemetry`.
- **Lazy telemetry. Breaking:** `SysinfoTelemetry` is no longer a unit struct.

  ```rust
  // Before
  let t = SysinfoTelemetry;
  // After
  let t = SysinfoTelemetry::new();
  ```

  The first snapshot returns a neutral CPU reading. A cold `run_with` drops from about 400 ms to
  about 1 ms.
- **Optional `compute` feature** (default on). With `default-features = false`, core builds
  without rayon or sysinfo, and bulk operations, streams and sorts run serially. `im` is no longer
  a dependency: `SpanRecord::annotations`, `TracingFiberRefs::span_annotations`,
  `Metric::snapshot_frequencies` and `Sink::collect_to_map` return a std `HashMap`, and
  `TimeBucketMap` is a `BTreeMap`.

## 8. `id_effect_axum`: the effect executor

Handlers no longer use `block_in_place`. Effects run on an `EffectExecutor`, an elastic pool of
dedicated threads: each in-flight request gets its own thread with a private single-threaded
runtime.

What changes for you:

- **`Send` bounds.** `run_with_env` and `run_with_caps` require `Send + 'static` closures, and
  `Send` success and error types. Build non-`Send` state (an `Rc`, a `RefCell`) inside the effect
  instead of capturing it.

  ```rust
  // Before: captured an Rc, compiled under block_in_place
  let cache = Rc::new(RefCell::new(HashMap::new()));
  run_with_env(move || lookup(cache.clone(), id), env)

  // After: shared state must be Send + Sync
  let cache = Arc::new(Mutex::new(HashMap::new()));
  run_with_env(move || lookup(cache.clone(), id), env)
  ```

- **Run to completion.** `DropPolicy::RunToCompletion` is the default: an effect finishes even if
  the client disconnects. Choose `DropPolicy::Cancel` to stop work when the response future is
  dropped.
- **Saturation.** Admission is bounded: `max_threads` (default 512) plus `queue_capacity`
  (default 1024). `SaturationPolicy::Wait` (default) queues. `SaturationPolicy::Reject` answers
  `503` with `Retry-After`.

  ```rust
  use id_effect_axum::{DropPolicy, EffectExecutor, SaturationPolicy};

  let executor = EffectExecutor::builder()
    .max_threads(64)
    .queue_capacity(256)
    .saturation(SaturationPolicy::Reject)
    .drop_policy(DropPolicy::Cancel)
    .build();
  ```

  A service can use its own executor via `EffectService::with_executor`.
- Current-thread Tokio runtimes now work. A blocking effect stalls only its own request.
  `tokio::spawn` inside an effect runs on an executor worker.
- New: request-scoped capability overrides (`CapOverrideLayer`, `RequestCaps`,
  `execute_with_request_caps`).

## 9. `id_effect_fsm`

The interpreter and sagas are Effect-native, and the old stubs are gone.

- **`Interpreter::step` / `run`** take the machine by value and return an `Effect` that yields the
  updated machine. The new state is kept only if the transition action succeeds. The environment
  comes from the effect's `R` instead of a cloned argument.

  ```rust
  // Before (0.4 / 0.5): mutate in place, run actions with run_blocking internally
  let state = interpreter.step(&mut machine, Event::Go, env.clone())?;

  // After: compose it, run it where you run other effects
  let machine = run_blocking(interpreter.step(machine, Event::Go), env)?;
  let state = machine.state();
  ```

- **`Saga::run` and `Saga::compensate_indices`** return `Effect`s:
  `saga.run(env)?` becomes `run_blocking(saga.run(), env)?`.
- **Session types removed** (`SessionSend`, `SessionRecv`, `PingPong`, …). There is no
  replacement in the published crates. Model protocols as state machines or process messages.
- **`#[derive(Fsm)]` is real now.** The old `id_effect::Fsm` stub derive is removed. The new one is
  in `id_effect_fsm`:

  ```rust
  use id_effect_fsm::Fsm;

  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  enum Light { Tick, Fault }

  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
  #[fsm(event = Light, initial = Red)]
  enum Signal {
    #[fsm(on(Tick => Green, Fault => Blinking))]
    Red,
    #[fsm(on(Tick => Amber))]
    Green,
    #[fsm(on(Tick => Red))]
    Amber,
    Blinking,
  }
  ```

  Unknown targets and duplicate edges are compile errors. A duplicate counts even when the same
  event is spelled two ways (`Go` and `E::Go`).
- **New:** `FsmProcess` / `FsmSpec` / `FsmHandle`, a `gen_statem`-style process behaviour that
  hosts a machine under a supervisor. `id_effect_fsm` now depends on `id_effect_process`.

## 10. `id_effect_process`

There are no signature changes, but some behaviour changed:

- Sending to a stopped process now returns `SendError::Rejected`. It used to succeed silently. Code
  that ignored send results may now see errors where it lost messages before.
- A panic in `init`, a handler or `terminate` is caught. The process exits with
  `ExitReason::Error("panic: …")`: links and monitors fire, pending calls fail with
  `CallError::Down`, and the supervisor restarts it. Before, callers timed out.
- New: `Addr<DynamicSupervisor>::terminate_dyn_child`.

## 11. `id_effect_node`: singleton lock keys

> **Rolling-upgrade warning.** `PgAdvisoryLease` derives its Postgres advisory-lock keys with
> SHA-256 now. Before, it used std's hasher, which may change between Rust versions. A 0.5 node
> and a 0.6 node compute **different keys for the same singleton name**, so both can believe they
> hold the lease. Do not run old and new builds against the same singleton. Stop every old node
> before starting new ones, or rename the singleton for the upgrade.

Other node changes are fixes. Replies and `Down` messages go only to the requesting peer
(`Mesh::handle_envelope_from`). Monitoring an unreachable node fires `NoConnection`. Failed
takeovers are kept as orphans and retried (`DistributedSupervisor::orphaned` /
`retry_orphaned`; `on_membership` now returns `Option<RetryHandle>`). QUIC no longer panics when
building its TLS config (`QuicConfig::with_server_name`, default `"cluster"`).

## 12. Other integration crates

- **`id_effect_platform`:** the `auth` module is behind the opt-in `auth` feature.
  `MemoryOAuthClient` needs the `test-util` feature. Only reqwest 0.13 is supported.

  ```toml
  id_effect_platform = { version = "0.6", features = ["auth"] }
  ```

- **`id_effect_sql`:** live Postgres tests fail when `ID_EFFECT_REQUIRE_PG=1` and no database is
  reachable. Otherwise they print `SKIP`.
- **`id_effect_resilience`:** new `retry_with_breaker` (retries on a `Schedule` and fails fast
  while the breaker is open), plus clock injection (`CircuitBreaker::make_with_clock`,
  `RateLimiter::make_with_clock`). `Bulkhead` rejects instead of waiting. Use core `Semaphore`
  when callers should wait.
- **`id_effect_optics`:** `transducer` re-exports core's transducer (core gains
  `transducer::take`).
- **`id_effect_config`** no longer depends on `id_effect_logger`.

## 13. Removed stubs and features

| Removed | Use instead |
|---------|-------------|
| `#[derive(Fsm)]` / `id_effect::Fsm` from core (a stub) | `id_effect_fsm::Fsm` derive ([section 9](#9-id_effect_fsm)) |
| generated `T::parser()` from the schema derive | `id_effect_parse::SchemaBridge::parser_for::<T>()` (labs) |
| the empty `algebra` feature of `id_effect` | nothing (`id_effect_fp` is always complete) |
| `id-effect` scaffolder binary and `id_effect_cli::generator` (stubs) | `examples/cli-minimal` as a template; `id-effect-diagnose` stays |
| fsm session types | state machines / process messages |
| `KafkaBrokerStub`, `NetworkJournalStub`, `SchemaBridgeStub` from published APIs | labs crates (test doubles only) |

`Monad::flatten` (now in `id_effect_fp`) has a real default implementation instead of calling
`unimplemented!`.

## 14. Capability ergonomics

0.6 removes most of the boilerplate around capabilities. One change is breaking (logger calls);
the rest are opt-in replacements for patterns that still compile.

### Logger calls lose the turbofish (breaking)

`EffectLogger` methods no longer take the environment type as a type parameter. An explicit
turbofish no longer compiles:

```rust
// Before
effect!(|r: &mut R| {
  ~logger.info::<R>("starting");
})

// After: inside effect!, the environment is inferred
effect!(|r: &mut R| {
  ~logger.info("starting");
})

// After: outside effect!, re-target the environment explicitly
let log = logger.info("starting").into_env::<R>();
```

### Providers: `#[provides(S, with = …)]`

The derive can take the build function directly, so you no longer need an inherent `new` that
returns a different type (and the `clippy::new_ret_no_self` allow that came with it):

```rust
// Before
#[derive(ProviderSpecDerive)]
#[provides(Greeter)]
struct GreeterLive;

impl GreeterLive {
  #[allow(clippy::new_ret_no_self)]
  fn new() -> Greeter { Greeter::english() }
}

// After
#[derive(ProviderSpecDerive)]
#[provides(Greeter, with = || Greeter::english())]
struct GreeterLive;
```

`with` also accepts a closure over the already-built dependencies (`with = |deps: &Env| …`) or a
path (`with = Self::build`).

### Infallible services: `widen_error`

To use an effect that cannot fail inside one with a real error type, replace the `unreachable!`
mapping with `widen_error`:

```rust
// Before
service.get().map_error(|()| -> AppError { unreachable!() })

// After
service.get().widen_error::<AppError>()
```

Prefer `Never` as the error type of infallible services. A `()` error still works, but if one is
ever produced, `widen_error` panics and reports the call site.

### Name capability sets once

Give a capability set a name and reuse it, including in `effect!` closures:

```rust
type AppCaps = caps!(Database, EffectLogger);

fn load_user(id: u64) -> Effect<User, DbError, AppCaps> {
  effect!(|r: &mut AppCaps| {
    let db = ~Database;
    ~db.get_user(id)
  })
}
```

### Missing-capability errors carry the call site

`Env::try_get` and `Env::get_any` report a missing capability as
`CapabilityError::MissingAt { cap, location }`, where `location` is the caller's source location
(via `#[track_caller]`). Code that matched `CapabilityError::Missing(cap)` should match both
shapes, most simply with `missing_capability()`:

```rust
match env.try_get::<Cap<Database>>() {
  Ok(db) => use_db(db),
  Err(err) => match err.missing_capability() {
    Some(cap) => eprintln!("not provided: {cap}"), // shows the service name
    None => return Err(err),
  },
}
```

`CapabilityId`'s `Debug` and `Display` now show the service name instead of an opaque id.

## 15. Checklist

- [ ] All `id_effect*` dependencies on `0.6`, published and shim crates together.
- [ ] Labs crates (`rpc`, `ai`, `parse`, `lint`, `workflow`, `events`, `jobs`, `graph`) switched to
      git/path dependencies, or removed.
- [ ] `id_effect_tower` → `id_effect_axum` + `tower` feature; `id_effect_sql_pg` →
      `id_effect_sql` + `pg` feature.
- [ ] `id_effect::{algebra, collections, stm}` and fp root re-exports → `id_effect_fp`.
- [ ] Placement imports → `id_effect_node::placement`; `scale_out_placed` is a free function.
- [ ] `ComputeSupervisor` → `FabricGovernor`, `platform::process` → `platform::command`,
      `derive(SchemaParser)` → `derive(Schema)` (no warning for the last one).
- [ ] `SysinfoTelemetry` → `SysinfoTelemetry::new()`.
- [ ] `Stream::from_iterable` gets owned iterators; no code polls past a stream error.
- [ ] axum closures and results are `Send + 'static`; executor limits and drop policy chosen.
- [ ] fsm `Interpreter::step` / `Saga::run` call sites run the returned `Effect`.
- [ ] No mixed 0.5/0.6 nodes share a `PgAdvisoryLease` singleton.
- [ ] Removed stubs (`derive(Fsm)` from core, `T::parser()`, `algebra` feature, scaffolder) are
      gone from your code.
- [ ] Logger calls have no `::<R>` turbofish; code matching `CapabilityError::Missing` also
      handles `MissingAt` (or uses `missing_capability()`).
