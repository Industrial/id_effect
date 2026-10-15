# Migrating to id_effect 0.7

0.7.0 is the runtime release (see [ADR 0011](adrs/0011-fiber-runtime-send-typed-r.md)). The
0.6 kernel (a `!Send`, busy-polled interpreter over a runtime-checked `Env`) is replaced by an
owned fiber runtime on an `Executor` trait, with tokio as the default backend:

- `Effect<A, E = Never, R = HNil>` is **`Send`**; `LocalEffect` holds `!Send` values.
- `R` is **typed**: an HList of capabilities (`HCons<Db, HCons<Logger, HNil>>`). A missing
  capability is a compile error, so `need::<T>()` can never fail at run time.
- Fibers have real wakers. Interruption stops work and runs finalizers, and a parent fiber
  interrupts its children when it exits.
- On top of the kernel, 0.7 adds the Effect.ts parity layer: `Layer` values, a typed
  `Schedule<Out, In>`, `race` / `timeout` / `zip_par` / `all`, request batching and caching,
  `Random`, a metrics registry and span hooks.
- The D8 prune removes the `id_effect_tower` and `id_effect_sql_pg` shims, the 0.6 deprecated
  aliases, unused root re-exports and the back-compat module aliases.

Everything ships as one breaking 0.7.0. The summary of changes is the `0.7.0` section of
[CHANGELOG.md](../CHANGELOG.md); this guide is the "if you used X, now do Y" version. It assumes
you are on 0.6.0. If you are on 0.4 or 0.5, apply [MIGRATION-0.6.md](MIGRATION-0.6.md) first.

Every "0.7" snippet below uses root paths (`id_effect::Effect`, `id_effect::Layer`, …).

Contents:

1. [Checklist](#1-checklist)
2. [Update the dependency](#2-update-the-dependency)
3. [The kernel](#3-the-kernel)
4. [Macros](#4-macros)
5. [Layers and scopes](#5-layers-and-scopes)
6. [Concurrency: parallel combinators, racing, the Fabric](#6-concurrency)
7. [Coordination primitives](#7-coordination-primitives)
8. [Resources: `Pool`, `KeyedPool`, `Cache`](#8-resources)
9. [Request batching and caching](#9-request-batching-and-caching)
10. [Streams](#10-streams)
11. [Schedule, clock and random](#11-schedule-clock-and-random)
12. [Observability: metrics, spans, the tracing buffer](#12-observability)
13. [Schema](#13-schema)
14. [Satellite crates](#14-satellite-crates)
15. [Labs crates](#15-labs-crates)
16. [Removed items and their replacements](#16-removed-items-and-their-replacements)

---

## 1. Checklist

- [ ] Bump every `id_effect*` dependency to `0.7` together. Replace `id_effect_tower` with
      `id_effect_axum` (feature `tower`) and `id_effect_sql_pg` with `id_effect_sql` (feature
      `pg`) ([§2](#2-update-the-dependency)).
- [ ] Replace `run_blocking(eff, env)` with `eff.provide_all(env).block_on()` at the program edge,
      and `run_async(eff, env).await` with `Runtime::run(..).await`. Both return an `Exit`; use
      `.into_result()` for a `Result<A, Cause<E>>` ([§3.2](#32-running-effects)).
- [ ] Replace `R = ()` with `R = HNil` (the default, so `Effect<A, E>`), and every `Env`,
      `Needs<K>` and `Cap<K>` with a typed `R` read with `need::<K>()` or `~K`
      ([§3.3](#33-typed-r)).
- [ ] Replace providers (`provide!`, `providers!`, `ProviderBox`, `build_env`, `run_with`) with
      `Layer`s and `provide_layer` ([§5](#5-layers-and-scopes)).
- [ ] `retry` / `repeat` take an effect **factory**: `Effect::retry(|| call(), schedule)`;
      `Schedule::compose` is `intersect` ([§11](#11-schedule-clock-and-random)).
- [ ] Coordination and resource constructors are plain functions: `Ref::new(0)`, not
      `Ref::make(0).run(..)` ([§7](#7-coordination-primitives), [§8](#8-resources)).
- [ ] Independent binds in `effect!` no longer run in parallel on their own; write
      `let (a, b) = ~(ea, eb);` ([§4.1](#41-effect-and-effect_local)).
- [ ] Match `Cause::Die(d)` as a `Defect` (`d.message()`), not a `String`
      ([§3.5](#35-failures-causedie-carries-a-defect)).
- [ ] Replace `TestClock` / `*_with_clock` / `run_test` with `TestExecutor` virtual time
      ([§3.2](#32-running-effects)).
- [ ] Replace the 0.6 `Metric` with `Counter` / `Histogram` / … and `with_span(eff, name)` with
      `eff.with_span(name)` plus a span hook ([§12](#12-observability)).
- [ ] Load configuration through `Config<T>` only ([§14.3](#143-id_effect_config)).
- [ ] Go through [§14](#14-satellite-crates) for each satellite crate you use, and
      [§16](#16-removed-items-and-their-replacements) for anything that no longer resolves.

## 2. Update the dependency

Every published crate moves to `0.7.0` in lockstep:

```toml
[dependencies]
id_effect = "0.7"
id_effect_tokio = "0.7"
# …every other id_effect_* crate you use, also "0.7"
```

The MSRV stays `rust-version = "1.94"`. tokio stays a mandatory dependency of `id_effect` (the
default executor backend, ADR 0010).

**Removed crates.** The 0.6 re-export shims are gone. Depend on their homes:

```toml
# 0.6
id_effect_tower = "0.6"
id_effect_sql_pg = "0.6"

# 0.7
id_effect_axum = { version = "0.7", features = ["tower"] }
id_effect_sql = { version = "0.7", features = ["pg"] }
```

Paths: `id_effect_tower::*` is `id_effect_axum::service::*`, and `id_effect_sql_pg::*` is
`id_effect_sql::pg::*`.

The published set is now 17 crates: `id_effect`, `id_effect_macro`, `id_effect_proc_macro`,
`id_effect_tokio`, `id_effect_config`, `id_effect_logger`, `id_effect_resilience`,
`id_effect_process`, `id_effect_node`, `id_effect_fsm`, `id_effect_platform`, `id_effect_axum`,
`id_effect_sql`, `id_effect_opentelemetry`, `id_effect_cli`, `id_effect_fp`, `id_effect_optics`.

**Cargo features.** The `proptest` feature of `id_effect` is removed together with the 0.6
`testing::{snapshot, proptest, stream}` helpers ([§16](#16-removed-items-and-their-replacements)).
`compute` (default) and `schema-serde` are unchanged.

## 3. The kernel

### 3.1 `Effect<A, E, R>` is `Send`; `LocalEffect` is the opt-in

`id_effect::Effect` is the 0.7 kernel's `Effect<A, E = Never, R = HNil>`. It is `Send`, and so
are the closures and values it captures. A body that has to hold an `Rc`, a `RefCell` or another
`!Send` value across an await builds a `LocalEffect` instead (`effect_local!`, `LocalEffect::*`).
A `LocalEffect` can bind `Send` effects, but it runs only on the calling thread
(`LocalEffect::block_on`), and it cannot fork.

```rust
// 0.6
use id_effect::{Effect, effect, run_blocking, succeed};
fn older(age: i64) -> Effect<i64, String, ()> {
  effect! { let a = ~succeed::<i64, String, ()>(age + 1); a }
}
assert_eq!(run_blocking(older(36), ()), Ok(37));

// 0.7
use id_effect::{Effect, Exit, effect, succeed};
fn bump(age: i64) -> Effect<i64, String> {
  succeed(age + 1)
}
fn older(age: i64) -> Effect<i64, String> {
  effect!({ ~bump(age) })
}
assert_eq!(older(36).block_on(), Exit::succeed(37));
```

The defaults changed: `E` defaults to `Never` and `R` to `HNil` (0.6 used `()` for both, by
convention). `Effect<A, E>` is an effect that needs nothing. The constructors are `succeed`,
`fail`, `die`, `sync`, `from_async`, `suspend`, `sleep` and `yield_fiber`, and the combinators
(`map`, `flat_map`, `map_err`, `catch`, `then`, `ensuring`, `exit`, …) keep their names.
`Effect::new(|r: &mut R| ..)` is gone: read capabilities with `need::<K>()` (§3.3).

### 3.2 Running effects

An effect is run at the **edge**: `main`, a test, or a framework callback. There are no free
`run_*` functions any more.

| 0.6 | 0.7 |
|---|---|
| `run_blocking(eff, env)` → `Result<A, E>` | `eff.provide_all(env).block_on()` → `Exit<A, E>` (`Effect::block_on` runs on `Runtime::global()`) |
| `run_async(eff, env).await` | `Runtime::global().run(eff.provide_all(env)).await`, or `TokioRuntime::current()?.run(..).await` ([§14.1](#141-id_effect_tokio)) |
| `run_fork(&rt, \|\| (eff, env))` | `eff.fork()` inside an effect, `Runtime::fork(eff)` at the edge → `FiberHandle` |
| `run_test(eff)`, `run_test_with_clock(eff, clock)` | a `TestExecutor` (FIFO, virtual time; below) |
| `ThreadSleepRuntime`, the v1 `Runtime` trait | `Runtime` (a value over an `Arc<dyn Executor>`), `TokioExecutor`, `TestExecutor` |
| `yield_now()` | `yield_fiber()` |

`Exit<A, E>` is `Exit::Success(a)` or `Exit::Failure(Cause<E>)`. `exit.into_result()` gives a
`Result<A, Cause<E>>`, and `cause.fail_option()` gives the typed failure.

**Blocking edges refuse to run inside async code.** `Effect::block_on` (and `Runtime::block_on`,
`LocalEffect::block_on`) parks the calling thread. Inside an async context (a tokio task or an
entered runtime) it runs nothing and returns a `Die` with the `BlockOnInAsync` message.
`try_block_on` returns that refusal as an `Err(BlockOnInAsync)`. In async code, await
`Runtime::run(..)` instead. 0.6's `run_blocking` nested a runtime in that case.

**Executors.** `Runtime::new()` owns a multi-thread tokio runtime with every driver enabled,
`Runtime::global()` is the process-wide default, and `Runtime::from_executor(exec)` runs on any
`Arc<dyn Executor>`: a `TokioExecutor` (`TokioExecutor::current()` inside a tokio runtime) or a
`TestExecutor`. Every fiber is a task with a real waker. Nothing busy-polls.

**Deterministic tests.** `TestExecutor` runs fibers in FIFO order on virtual time. Sleeps,
timeouts, schedule delays and pool TTLs all read the executor clock, so they fire when you
advance it:

```rust
// 0.6
let clock = TestClock::new();
let exit = run_test_with_clock(retry_with_clock(|| call(), schedule, clock.clone()), clock);

// 0.7
use std::sync::Arc;
use id_effect::{Runtime, TestExecutor};
let exec = TestExecutor::new();
let rt = Runtime::from_executor(Arc::new(exec.clone()));
let handle = rt.fork(Effect::retry(|| call(), schedule));
exec.run_until_stalled();
exec.advance(Duration::from_secs(5)); // fires the sleeps that are due
exec.run_until_stalled();
```

### 3.3 Typed `R`

The 0.6 `Env` (a runtime type map read with `Needs<K>` / `Cap<K>`) is gone. `R` is an HList
of capability values, and every read is checked at compile time:

```rust
// 0.6
fn users(env: &mut Env) -> Effect<Vec<User>, DbError, Env> {
  let db = Needs::<Db>::need(env).clone();
  db.list_users()
}
let out = run_blocking(users_effect(), build_env([provide!(DbLive)])?);

// 0.7
use id_effect::{Effect, HCons, HNil, caps, hlist, need};
fn users() -> Effect<Vec<User>, DbError, caps!(Db)> {
  // caps!(Db) is HCons<Db, HNil>; list_users() needs nothing, so narrow it into caps!(Db)
  need::<Db>().flat_map(|db| db.list_users().narrow())
}
let exit = users().provide(Db::connect()).block_on();
let exit = users().provide_env(hlist![Db::connect()]).block_on();
```

- `need::<K>()` reads a capability (an `Arc<K>`) from `R`. `need::<K>().flat_map(f)` runs `f`'s
  effect over the same `R`, so lift an effect that needs less with `.narrow()`. A generic helper
  names the bound `R: Selector<K, I>` with an extra index parameter `I`, which is inferred (one
  more `_` in a turbofish).
- `provide(k)` provides the head of `R`, `provide_env(hlist![..])` / `provide_all(env)` provide the
  whole environment (`provide_env` accepts the capabilities in any order), and `narrow()` lifts an
  effect over a smaller `R` into a larger one. A sub-effect bound with `~` inside `effect!` is
  narrowed for you.
- `.same_env()` marks a generic helper that runs over the caller's whole environment.
- **Named capability variants** (0.6 `#[named(..)]`) are gone. Use distinct newtypes, or
  `DynEnv`, the runtime-keyed environment that `id_effect_axum` uses for per-request
  capabilities (`DynEnv::new().with(x)`, `env.try_get::<T>()`).

### 3.4 Effects run once

A 0.7 `Effect` is a one-shot value: its steps are `FnOnce`, and it is not `Clone`. Anything that
runs an effect more than once takes a **factory** (`Fn() -> Effect`) that builds a fresh one per
run: `Effect::retry` / `repeat` (§11), `Layer::effect` (§5), `retry_with_breaker` (§14.7) and
`Supervisor::spawn` (§7).

### 3.5 Failures: `Cause::Die` carries a `Defect`

`Cause::Die` carries a `Defect`: the message, plus the original payload for panics
(`Defect::from_panic`, `Cause::from_panic`). `Cause::die` / `Exit::die` still accept `&str` and
`String`. `Eq` / `Hash` compare the message only.

```rust
// 0.6
Cause::Die(msg) => eprintln!("{msg}"),          // msg: String

// 0.7
Cause::Die(d) => eprintln!("{}", d.message()),  // d: Defect (also Display)
let p = d.take_payload();                       // Option<Box<dyn Any + Send>>
d.with_payload(|e: &MyPanic| e.code);           // downcast in place
```

A panic in any fiber, finalizer or run edge surfaces as `Cause::Die` with a downcastable
payload; it no longer unwinds through the caller. A body that succeeds with a panicking finalizer
ends in a plain `Die`.

### 3.6 Fibers and interruption

- `eff.fork()` returns a `FiberHandle<A, E>` (`join()`, `join_future()`, `interrupt()`,
  `interrupt_now()`). A forked child is interrupted when its parent exits.
  `fork_all(effects, concurrency)` forks and joins a `Vec`.
- `eff.fork_daemon()` detaches the lifetime only. The daemon now starts with a **copy of the
  forking fiber's `FiberRef` values** at fork time, like Effect.ts `forkDaemon` (0.6 started it
  from the initial values). A `set` on either side stays invisible to the other.
- Interruption replaces cooperative cancellation. `CancellationToken`, `check_interrupt` and
  `interrupt_all` are gone. Interrupting a fiber stops it at its next suspension point and runs
  its finalizers. `uninterruptible()` / `interruptible()` and `uninterruptible_mask` scope it.
- `FiberRef<T>` is a plain value (`FiberRef::new(init)`), with `get` / `set` / `update` /
  `locally` effects. Each run edge starts a root fiber from the ref's initial value.
- `Effect::scoped(|scope| ..)`, `acquire_use_release` and `ensuring` replace the free `scoped`,
  `scope_with` and `acquire_release` functions.

### 3.7 Module paths

Every kernel module is a root module, and its items are re-exported at the crate root:
`kernel`, `runtime`, `capability`, `concurrency`, `coordination`, `resource`, `streaming`,
`testing`, `scope`, `layer`, `par`, `race`, `random`, `request`. `scheduling` is `duration` +
`datetime` (unchanged) + `Schedule`, `observability` is `LogLevel` (unchanged) + metrics, spans
and the tracing buffer, and `compute` is the kernel's `Fabric` (always built) + the Compute
Fabric substrate (feature `compute`, as before). Import from the root where you can.

- A 0.6 path into a module that still exists now names the 0.7 module. `id_effect::runtime::Never`
  no longer resolves: import `id_effect::{Chunk, FiberId, Never}` from the root.
- The back-compat module aliases `id_effect::{channel, ref_, either, func, clock, duration,
  schedule, data, stream, snapshot}` are removed:

```rust
// 0.6                                  // 0.7
use id_effect::duration::Duration;      use id_effect::Duration; // or id_effect::scheduling::duration::Duration
use id_effect::data::EffectData;        use id_effect::schema::data::EffectData;
use id_effect::stream::StreamSender;    use id_effect::StreamSender;
use id_effect::either::Either;          use id_effect::Either;
use id_effect::channel::QueueChannel;   use id_effect::Channel; // see §7
```

- `pipe1` / `pipe2` / `pipe3`, `flip`, `const_`, `tupled`, `untupled`, `memoize` and `MutableRef`
  are removed from core; `identity`, `always` and `compose` stay. `id_effect_fp` keeps its own
  copies: `use id_effect_fp::foundation::function::{pipe2, flip};`. Replace `MutableRef` with
  `std::sync::Mutex`, a `FiberRef` or a `SynchronizedRef`.
- The 0.6 deprecated aliases are removed: `StreamV1` is `Stream`, `ComputeSupervisor` is
  `FabricGovernor`, `fabric.supervisor()` is `fabric.governor()`, and
  `id_effect_platform::process::CommandSpec` is `id_effect_platform::command::CommandSpec`.

## 4. Macros

| 0.7 root macro | 0.6 | notes |
|---|---|---|
| `effect!` | `effect!` (do-notation over an `Env`) | same `~` syntax over a typed `R`: `~Cap` reads a capability, `~expr` binds, `~(a, b)` joins |
| `effect_local!` | new | like `effect!`, but builds a `LocalEffect` (`!Send` body) |
| `caps!(A, B)` | `caps!` (built a `CapList`) | the type `HCons<A, HCons<B, HNil>>` |
| `derive(ProviderSpecDerive)` | the same derive | emits only an inherent `layer()` (§4.3) |
| `pipe!`, `err!`, `match_effect!`, `effect_tagged`, `EffectData`, `Schema`, `Optics` | unchanged | kernel-neutral |
| `hlist![a, b]` | new | an `HCons` value, for `provide_env` / `provide_all` |

### 4.1 `effect!` and `effect_local!`

```rust
// 0.6
fn program() -> Effect<u32, EffectLoggerError, caps!(EffectLogger)> {
  effect!(|r| {
    let logger = *~EffectLogger;
    ~logger.info("all good");
    7
  })
}

// 0.7
use id_effect::{Effect, caps, effect};
fn program() -> Effect<u32, EffectLoggerError, caps!(EffectLogger)> {
  effect!({
    let logger = ~EffectLogger; // a reference to the capability
    ~logger.info("all good");
    7
  })
}
```

- The body forms are `effect!({ .. })`, `effect!(|| { .. })` and
  `effect!(|r: &caps!(A, B)| { .. })`, which binds the environment by reference.
- A bound effect's error is widened into the body's error with `From`, as in 0.6, and `?` on a
  `Result` fails the body.
- `require!` is gone: write `~Key` (or `need::<Key>()` outside `effect!`).
- **Parallel binds are explicit.** 0.6's `effect!` ran independent `let x = ~a; let y = ~b;`
  statements in parallel on its own (`join_binds2..4`, with an opt-out `#[serial]`). In 0.7
  binds are sequential, as in Effect.ts's `Effect.gen`. Write a parenthesised list after `~`
  (2 to 8 effects) to run them in parallel:

```rust
// 0.6: implicit, up to 4 independent binds
let user = ~load_user(id);
let orders = ~load_orders(id);

// 0.7: explicit; a tuple in input order
let (user, orders) = ~(load_user(id), load_orders(id));
```

The branches run as child fibers (`all` with `Concurrency::Unbounded`). The first failure
interrupts the siblings that are still running, the join waits for them (their finalizers run),
and that first failure is the outcome of the `~`. To inspect every sibling failure, bind
`~zip_par(a, b).exit::<Never>()` instead. `~(expr)` with no top-level comma is a plain bind.
`effect_local!` rejects `~(a, b, ..)`, because its branches would have to be `Send` to fork.

### 4.2 The removed capability-DI macros

`id_effect_macro`'s `caps!`, `provide!`, `providers!`, `require!` and `mock_capability!` built
0.6 capability types (`Env`, `CapList`, `Cap`, `ProviderBox`, `ProviderSpec`, `Needs`). They are
deleted. `pipe!` and `err!` stay.

| 0.6 macro | 0.7 replacement |
|---|---|
| `caps!` (`macro_rules!`, a `CapList`) | the root proc macro `caps!(A, B)`, an `HCons` type |
| `provide!(P)` | a `Layer`: `P::layer()`, `Layer::succeed(hlist![..])` |
| `providers![P, Q]` | `P::layer().merge(Q::layer())` |
| `mock_capability!` | a plain value: `.provide(mock)`, `.provide_env(hlist![mock, ..])`, `Layer::succeed(hlist![mock])` |
| `require!(Key)` | `~Key` in `effect!`, `need::<Key>()` elsewhere |

### 4.3 `derive(ProviderSpecDerive)`

The derive keeps its name and attributes but emits only an inherent `Struct::layer::<E>()`, a
`Layer<HCons<Service, HNil>, E>` built with the same constructor. The 0.6 `impl ProviderSpec`,
the `with = |deps: &Env| ..` constructors and the `#[named(..)]` variants are compile errors.
`on_shutdown` is replaced by `release = <fn(Arc<Service>) -> Effect<(), Never, HNil>>` in
`#[provides(..)]`, an async finalizer that runs when the layer's scope closes:

```rust
use std::sync::Arc;
use id_effect::{Effect, Never, HNil, ProviderSpecDerive, sync};

fn close_pool(pool: Arc<Pool>) -> Effect<(), Never, HNil> {
  sync(move || pool.close())
}

#[derive(ProviderSpecDerive)]
#[provides(Pool, with = || Pool::open("mem://"), release = close_pool)]
pub struct PoolLive;

let exit = use_pool().provide_layer(PoolLive::layer::<AppError>()).block_on(); // closed afterwards
```

The derive adds an inherent `layer` function, so a type that already defines its own `layer`
must rename one of them.

### 4.4 `#[derive(SchemaParser)]` and `#[capability]`

Both were deprecated no-op or alias macros in 0.6 and are removed:

```rust
// 0.6                                       // 0.7
#[derive(Clone, SchemaParser)]               #[derive(Clone, Schema)]
struct User { name: String }                 struct User { name: String }

#[id_effect::capability(Db)]                 // delete the attribute: the service type is the
struct DbKey;                                // key: caps!(Db), need::<Db>(), #[provides(Db)]
```

Text parsers: `id_effect_parse::SchemaBridge::parser_for::<User>()` (labs).

## 5. Layers and scopes

The 0.6 provider graph (`ProviderSpec`, `ProviderBox`, `CapabilityGraph`, `build_env`,
`run_with`) is replaced by `Layer<Out, E, In = HNil>` values (Effect.ts `Layer`): a recipe that
needs the environment `In`, may fail with `E`, and produces the capabilities `Out`. A missing
capability is a compile error, so there is no runtime planner to call.

```rust
// 0.6
let env = build_env([provide!(LogMetadataLive), provide_config_provider(p)])?;
let out = run_with([provide!(DbLive)], program())?;

// 0.7
use id_effect::{Layer, caps, hlist};
let config: Layer<caps!(Settings), AppError> = Layer::succeed(hlist![Settings::default()]);
let db: Layer<caps!(Db), AppError, caps!(Settings)> = Layer::effect(|| connect_db()); // rebuildable
let app = config.provide_to(db);         // Settings feeds Db: Layer<caps!(Db), AppError>
let exit = program().provide_layer(app).block_on();
```

- Constructors: `Layer::succeed(out)`, `Layer::effect(|| eff)` (a factory: built again on each
  build), `Layer::from_effect(eff)` (one-shot: a second build fails), `Layer::scoped(|scope| ..)`
  and `Layer::acquire_release(acquire, release)`.
- Composition: `a.merge(b)` (both outputs, one shared `E`), `lower.provide_to(upper)` (feeds
  `upper`), `provide_merge` (feeds and keeps both outputs), and `effect.provide_layer(layer)`.
  Merged layers build sequentially. A merge with two layers that produce the same type panics.
- **Memoised by identity.** A `Layer` is a cheap, cloneable shared value. Within one build, the
  same layer value (or a clone of it) is built once and shared by everything that needs it. A
  failed construction is not memoised. `layer.fresh()` builds its graph under a new memo table
  (Effect.ts `Layer.fresh`).
- The layers that satellite crates export need nothing (`In = HNil`) and never fail, so their
  `E` is free: it is the error type of the graph they join.

### Scoped layers replace `on_shutdown`

A build has one `Scope`. Layers register finalizers into it, and they run once,
uninterruptibly and in reverse order of acquisition, when the scope closes:

```rust
// 0.6: a sync hook, run by run_with after the program
impl ProviderSpec for PoolLive {
  // ...
  fn on_shutdown(pool: &Pool) { pool.close_blocking() }
}

// 0.7: an async finalizer effect, tied to the layer's scope
let pool: Layer<caps!(Pool), DbError> = Layer::acquire_release(
  || connect(),                      // runs uninterruptibly
  |env, _kind| close(env.0),         // Effect<(), Never, HNil>, may await
);
program.provide_layer(pool)          // released after `program`, however it ends
```

- `Layer::scoped(|scope| ..)` hands the build's `Scope` to the construction, which can call
  `scope.add_finalizer(..)` / `add_finalizer_exit(..)`. `release(out, kind)` receives the
  closing `ExitKind`.
- **`Effect::provide_layer` owns the layer's scope.** It closes after the effect completes
  (success, failure, defect or interrupt), and also when the build itself fails or is
  interrupted (Effect.ts `Effect.provide(layer)`).
- `layer.build_scoped(&scope)` builds into the caller's scope (Effect.ts `Layer.buildWithScope`),
  and `layer.launch()` builds and then waits until it is interrupted, which closes the scope
  (Effect.ts `Layer.launch`).
- `layer.build()` has no owner for its scope. A failed or interrupted build releases what it had
  acquired, but **a successful `build()` never runs its finalizers**. Use one of the owners above
  for layers that hold resources.

**`Scope`.** `Effect::scoped(|scope| ..)` runs a body with a fresh `Scope` that closes when the
body ends. `scope.add_finalizer(eff)` / `add_finalizer_exit(|exit| eff)` replace the 0.6
`Finalizer`. Pools, `PubSub`, `SubscriptionRef` and `Semaphore::acquire_scoped` take the scope as
an argument (`pool.get(&scope)`) instead of reading a `Scope` from the environment.

## 6. Concurrency

### Parallel combinators: `zip_par`, `all`, `for_each_par`

0.7 adds structured parallel combinators on fibers (Effect.ts `Effect.all` / `Effect.forEach`
with `concurrency`). They replace a hand-rolled `fork` + `fiber_all`:

```rust
use id_effect::{Concurrency, all, for_each_par, zip_par, zip_with_par};
zip_par(load_user(id), load_orders(id))                    // Effect<(User, Vec<Order>), E, R>
zip_with_par(a, b, |a, b| a + b)
all(effects, Concurrency::Bounded(4))                      // Vec<A>, input order
for_each_par(ids, |id| fetch(id), Concurrency::Unbounded)  // Vec<A>, input order
all(effects, Concurrency::Sequential)                      // a left fold, nothing forked
```

- The first failure interrupts the siblings in flight and starts no further ones. The result is
  returned only after every started child has finished (its finalizers have run).
- The cause is that first failure. A sibling that also fails genuinely (`Fail` or `Die`, for
  example inside an uninterruptible region or in its finalizer) is added as
  `Cause::Both(first, other)`. The interrupts of siblings stopped by the first failure are not
  added, so a lone failure is exactly `Cause::Fail(e)`.
- `Concurrency::Bounded(n)` is a local window. `Concurrency::Inherit` takes its limit from the
  ambient `Fabric` (below).

### Racing and timeouts

```rust
use std::time::Duration;
use id_effect::race_all;
slow.race(fast)                                  // Effect<A, E, R>: first success wins
race_all([a, b, c])                              // first success among many
a.race_first(b)                                  // first completion wins, success or failure
call.timeout(Duration::from_secs(2))             // Effect<Option<A>, E, R>: None on timeout
call.timeout_fail(Duration::from_secs(2), || AppError::Timeout) // fails with your error
```

- Every racer runs as a child fiber. The losers are interrupted and awaited, so their finalizers
  have run before the result is returned.
- `race` / `race_all`: a failure does not end the race. When every racer fails, the cause is
  every failure combined with `Cause::Both` in input order. Unlike Effect.ts `raceAll` (last
  failure only), `race_all` keeps them all, so `race_all([a, b])` and `a.race(b)` agree.
  `race_all` of no effects dies.
- `timeout(d)` returns `Option<A>` instead of failing with a timeout error; use `timeout_fail`
  for a typed error. The deadline is the fiber's executor clock (virtual on `TestExecutor`). An
  uninterruptible effect delays its timeout until it completes; the result is still `None`.

### The Fabric as scheduler: `Concurrency::Inherit`

In 0.6 the Compute Fabric admitted work by blocking a thread
(`AdmissionController::acquire_blocking`, used by `ComputeFabric::spawn_admitted`). In 0.7 the
fiber runtime admits asynchronously, and only for `Concurrency::Inherit` (Effect.ts
`concurrency: "inherit"`):

```rust
use std::sync::Arc;
use id_effect::{Concurrency, Fabric, all, for_each_par};
let fabric = Fabric::with_capacity(2);              // or Fabric::from_compute(Arc::clone(&compute_fabric))
fabric.provide(all(effects, Concurrency::Inherit))  // at most 2 children run at once
fabric.provide(for_each_par(ids, fetch, Concurrency::Inherit))

all(effects, Concurrency::Inherit)                  // no ambient fabric: Unbounded
fabric.provide(all(effects, Concurrency::Bounded(8))) // a local window of 8; the fabric is not consulted
```

- The ambient fabric is fiber-local (like Effect.ts `Effect.withConcurrency`): `Fabric::provide`
  sets it for an effect and the children it forks, and `Fabric::current()` reads it. It is not a
  capability in `R`, and the process-wide `id_effect::compute::install_fabric` is not consulted.
- Each `Inherit` child holds one permit while it runs and gives it back however it exits. A
  throttled child is a suspended future, not a parked thread. Nested `Inherit` forks do not
  deadlock: a fiber that holds a permit lends it to one of its own `Inherit` children at a time.
- `AdmissionController::admit()` / `admit_n(n)` and `ComputeFabric::admit()` are the async
  admissions (an `AdmissionGuard` that releases on drop). A custom admission implements
  `Admission` and is wrapped with `Fabric::new`.
- `id_effect_tokio::TokioRuntime` no longer owns or installs a Fabric (`with_fabric` and
  `fabric()` are removed).

## 7. Coordination primitives

`Deferred`, `Semaphore`, `Latch`, `Queue`, `Channel`, `PubSub`, `Ref`, `SynchronizedRef`,
`SubscriptionRef` and `Supervisor` run on the fiber kernel. A waiting operation (acquire, take,
offer, wait) suspends the fiber on a waker: no thread is blocked, and nothing is built on tokio
`watch` / `Notify` / `Semaphore` / `Mutex` or flume any more.

```rust
// 0.6
let r = Ref::make(0).run(&mut ())?;           // constructors were effects
let sem = Semaphore::make(2).run(&mut ())?;
let q = Queue::<u32>::bounded(8).run(&mut ())?;
let d = Deferred::<u8, E>::make().run(&mut ())?;

// 0.7
use id_effect::{Channel, Deferred, Latch, PubSub, Queue, Ref, Semaphore, SubscriptionRef, Supervisor, SynchronizedRef};
let r = Ref::new(0);                           // plain constructors
let sem = Semaphore::new(2);
let q = Queue::<u32>::bounded(8);
let d = Deferred::<u8, E>::new();
let l = Latch::new();                          // `Latch::with_state(true)` starts open
let s = SynchronizedRef::new(0);
let sr = SubscriptionRef::new(0);
let ps = PubSub::<u32>::bounded(16);
let ch = Channel::<u32>::duplex_unbounded();
let sup = Supervisor::new();
```

- **Constructors are plain functions**; the free-function forms (`ref_::make`, `latch::open`,
  `semaphore::acquire`, …) are gone. Operations stay lazy effects, generic over the error type
  and environment where they cannot fail (`r.get::<MyErr, _>()`, or let inference pick them).
- **Interruption.** Interrupting a waiting fiber removes it from the primitive. A permit or a
  queue item handed to it but not yet observed goes back (`Semaphore`, `Queue`, partial `take_n`
  results included); an interrupted offer stores nothing.
- **`Deferred`**: `wait()` re-raises the stored cause as is (`Effect<A, E, R>`, not
  `Effect<A, Cause<E>>`), and `interrupt()` uses the calling fiber's id. `complete(eff)` now stores
  `eff`'s outcome **whether it succeeds or fails** (Effect.ts); 0.6 returned the failure without
  completing. `try_done(exit)` joins `try_succeed` / `try_fail_cause`, and `wait_future()` returns
  an `Exit<A, E>`.
- **`Semaphore`**: `acquire()` / `acquire_n(n)` return an owned `Permit` that releases on drop
  (0.6 `acquire_owned`). The 0.6 `acquire` under a `Scope` environment is
  `acquire_scoped(&scope)`. Prefer `with_permits(n, eff)` / `with_permit(eff)`, which release
  however `eff` ends. Waiters are FIFO: a large `acquire_n` at the front blocks later small ones.
- **`Queue`**: on a full **bounded** queue `offer` now suspends until a slot frees up (Effect.ts
  back-pressure). The 0.6 non-suspending `offer` is `try_offer`, and the 0.6 `offer_all`
  (returning the overflow) is `try_offer_all`. `offer_all` suspends on a bounded queue and returns
  the items that were not stored, including the ones offered after `shutdown` (0.6 dropped those
  silently). The `take_*` family returns `Vec<A>` instead of `Chunk<A>`. Shutdown is unchanged:
  buffered items stay readable, then takers fail with `QueueError::Disconnected`, and suspended
  offers return `false`.
- **`PubSub`**: `subscribe(&scope)` takes the scope as an argument (0.6 ran under a `Scope`
  environment) and returns a per-subscriber `Queue` with the hub's strategy. A bounded hub's
  `publish` suspends until every subscriber has room; `size` / `is_full` describe the fullest
  subscriber. `publish` still returns `false` with no subscriber.
- **`SubscriptionRef`**: `subscribe(&scope)` likewise. The queue gets the current value, then every
  later value in order with no gap. `modify` is new.
- **`SynchronizedRef`** is serialized through a one-permit `Semaphore` instead of a tokio mutex. An
  interrupted effectful update releases the lock and leaves the value unchanged.
- **`Latch`** adds `close`, `release` (wake the current waiters without opening) and
  `when_open(eff)`.
- **`Channel<Out, In = Out>`** is the queue-backed duplex channel (`duplex_unbounded`,
  `duplex_bounded`, `from_queue_and_map`, `map_in`, `map_out`, `flat_map_out`, `pipe`, `read`,
  `write`, `shutdown`, `to_stream`, `to_sink`). It replaces the 0.6 `QueueChannel`. `read()` yields
  `None` once the queue is shut down and drained, and read errors no longer carry
  `ChannelReadError`. The stream-, sink- and fold-backed 0.6 channels are separate types now
  (`StreamChannel`, `FoldChannel`, [§10](#10-streams)).
- **`Supervisor`** owns a `Scope`: `spawn(policy, make)` forks the `supervised(policy, make)`
  loop into it, and `shutdown()` (or closing a parent it was `attach_to`'d) interrupts every
  supervised fiber. `CancellationToken` and the explicit clock are gone: interruption replaces
  cooperative cancellation, and delays use the executor timer. `SupervisorPolicy::Restart` and
  `RestartWithLimit` take a fixed `delay: Duration` (`Duration::ZERO` yields once). Defects are
  restarted like typed failures; an interrupt never is. `RestartIntensity` is unchanged.

## 8. Resources

`Pool`, `KeyedPool` and `Cache` (Effect.ts `Pool` / `KeyedPool` / `Cache`) have plain
constructors. They are no longer `make() -> Effect`:

```rust
// 0.6
let pool = run_blocking(Pool::make(4, connect), ())?;
let cache = run_async(Cache::make(128, Some(ttl), lookup), ()).await?;

// 0.7
use id_effect::{Cache, Effect, KeyedPool, Pool};
let pool = Pool::new(4, connect).with_ttl(idle_ttl).with_release(close);
let keyed = KeyedPool::new(2, |host: Host| connect_to(host));      // 2 per key
let cache = Cache::new(128, Some(ttl), lookup);

Effect::scoped(move |scope| pool.get(&scope).flat_map(use_conn))  // checked in on close
cache.get(key)                                                     // Effect<V, E, R>
```

- `Pool::get(&scope)` takes the scope as an argument. The item is checked back in by a scope
  finalizer, however the scope closes (interrupt included). Waiting for capacity is
  interruptible and leaks no capacity; acquiring a new item runs uninterruptibly, as in Effect.ts
  `acquireRelease`. A failing acquire gives its capacity back.
- `Pool<A, E, R>` / `KeyedPool<K, A, E, R>` / `Cache<K, V, E, R>`: the acquire and lookup effects
  may need capabilities `R`, and `get` then needs the same `R`. `E` defaults to `Never` and `R`
  to `HNil`.
- New: `with_release(hook)` disposes of an item that is invalidated, expires, or outlives
  `shutdown()`. `shutdown()` is an infallible `HNil` effect, so
  `scope.add_finalizer(pool.shutdown())` ties a pool's lifetime to a scope. After shutdown,
  checkouts are interrupted.
- `make_with_ttl(cap, ttl, f)` is `new(cap, f).with_ttl(ttl)`. An idle item is released once it
  has been idle for `ttl`, read from the executor clock.
- **`KeyedPool` capacity is per key** (Effect.ts). In 0.6 it was one global capacity shared by all
  keys. `KeyedPool::new_with(|key| n, acquire)` sets a capacity for each key.
- **`Cache` caches typed failures** for the TTL, as Effect.ts does with the lookup's `Exit` (0.6
  cached only successes). Defects reach the callers but are not cached. An interrupted lookup is
  not cached either, and the callers waiting on it retry. `Cache::get` fails with `E`, not
  `Cause<E>`; the full cause still propagates.
- `Cache::invalidate` no longer interrupts the callers waiting on a lookup in flight: they still
  get its result, but it is not stored. New: `set`, `refresh`, `get_if_present`, `contains`,
  `invalidate_all` and `size`. `stats()` reports `{ hits, misses, size }` (Effect.ts `CacheStats`);
  0.6's `evictions` and `loads` are gone.
- `capacity == 0` still means unbounded, and least-recently-used eviction is unchanged.

## 9. Request batching and caching

The 0.6 `request_resolver` module (`RequestEntry`, `FnRequestResolver`,
`make_request_resolver`, `batching`, `MissingKey`, `BatchError`) is replaced by the `request`
module (Effect.ts `Effect.request` / `RequestResolver.makeBatched`):

```rust
use id_effect::{Concurrency, Effect, Exit, Request, RequestResolver, all, sync};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GetUser(u32);

impl Request for GetUser {
  type Output = String;
  type Error = String;
}

let users = RequestResolver::make_batched(|reqs: Vec<GetUser>| {
  sync(move || reqs.into_iter().map(|GetUser(id)| Ok(format!("user-{id}"))).collect())
});

// Three requests started together: one resolver call with the keys {1, 2}.
let names = all([1, 2, 1].map(|id| Effect::request(GetUser(id), &users)), Concurrency::Unbounded);
assert!(matches!(names.block_on(), Exit::Success(v) if v == ["user-1", "user-2", "user-1"]));
```

- A `Request` is its own key (`Clone + Eq + Hash`) and names its typed `Output` and `Error`. The
  resolver returns one `Result` per request, in order, so one request can fail with
  `Cause::Fail(e)` while the rest of the batch succeeds. A resolver effect that fails with `e`
  fails every request of the batch with that cause. A resolver that returns the wrong number of
  results, or panics, fails the batch with `Cause::Die`. Nothing panics on a missing key.
- The resolver's effect is awaited on a fiber: no `run_blocking`, so it is safe on a
  `current_thread` runtime.
- Batching is deterministic on `TestExecutor` and best-effort on a multi-thread executor; results
  never depend on how requests were grouped.
- **Caching** is off by default (Effect.ts). A `RequestCache` (FIFO, default capacity 65 536)
  remembers results, typed failures included, across batch windows: `eff.with_request_cache(&cache)`,
  `cache.provide(eff)`, `cache.provide_scoped(&scope)`, `cache.layer()`, and
  `eff.with_request_caching(bool)`. Providing a cache also turns caching on (in Effect.ts these are
  two steps). There is no TTL.

## 10. Streams

`Stream`, `Sink` and the channel adapters run on the fiber kernel. A stream is an owned pull:
each pull returns one chunk **and the rest of the stream**, so there is no shared
`Arc<Mutex<StreamState>>` locked on every pull. Operators run under the consumer's fiber and
environment (no `R` clone), and a running stream is interrupted like any other effect.

```rust
// 0.6
let out = pollster::block_on(
  Stream::from_iterable(0..10)
    .map(|n| n * 2)
    .filter(Box::new(|n: &i32| n % 3 == 0))
    .map_effect(|n| succeed::<i32, (), ()>(n + 1)) // Stream<i32, Or<(), ()>, ()>
    .run_collect()
    .run(&mut ()),
)?;

// 0.7
use id_effect::{Stream, succeed};
let out = Stream::<i32, MyErr>::from_iterable(0..10)
  .map(|n| n * 2)
  .filter(|n: &i32| n % 3 == 0)                  // any `Fn(&A) -> bool`, no `Box`
  .map_effect(|n| succeed(n + 1))                 // sequential, same `E`
  .run_collect()
  .block_on()
  .into_result()?;
```

- **Type parameters.** `Stream<A, E = Never, R = HNil>` (0.6: `E = (), R = ()`), like `Effect`.
  Constructors are generic over `E` and `R`; annotate one of them
  (`Stream::<u32, MyErr>::from_iterable(..)`) when nothing else fixes them.
- **`map_effect` is sequential**: one element's effect finishes before the next starts, so side
  effects happen in stream order. 0.6 ran every element of a chunk concurrently under the Fabric
  admission budget. For concurrency use **`map_effect_par(n, f)`**: at most `max(n, 1)` effects
  in flight, as child fibers, with the output in stream order. `map_effect`'s error is the
  stream's `E` (0.6: `Or<E, E2>`); map the effect's error with `.map_err(..)` inside `f`. `f` may
  be `FnMut`.
- **Pulling by hand.** `poll_next_chunk(&mut self, &mut env)` becomes `next_chunk(self)` (and
  `next_chunk_with_size(self, n)`), an effect that yields
  `Option<(Chunk<A>, Stream<A, E, R>)>`. A failed pull leaves no rest, so a stream is fused by
  construction. A size of `0` counts as `1` (0.6 returned `None` without consuming).
- **Sources.** `Stream::new(|env| fut)` is gone. Use `from_effect(effect)` (single batch),
  `from_pull(|n| effect)` (incremental; the pull returns an `Effect` of an optional `Vec`, not a
  boxed future over `&mut R`), `unfold_effect`, `from_queue(queue)`, `Stream::from_channel(&channel)`
  or `channel.to_stream()`. New: `succeed`, `fail`, `from_vec`, `from_chunk`, `flat_map`, `concat`,
  `map_err`, `widen`, `run_drain`, `run(sink)`.
- **Cancellation.** 0.6 consumers stopped early when `R` embedded a cancelled
  `CancellationToken`. 0.7 streams are interrupted through their fiber (`fork` + `interrupt`).
  Pulls that produce empty chunks yield to the executor, so they stay interruptible.
- **`stream_from_channel` / `StreamSender`.** `send_chunk(&sender, chunk)` is an effect, generic
  over `R`, that **suspends** while a `BoundedBlock` queue is full (0.6 spun `run_blocking` +
  `yield_now`). Run it from a fiber, or `.block_on()` it on a producer thread.
  `end_stream(sender)` is an `Effect<(), Never, R>`. `StreamSender::fail(e)` still returns `bool`
  but **never waits**: the error is delivered after every chunk already sent. Dropping the last
  sender now ends the stream (0.6 left the consumer waiting). `A` and `E` no longer need `Clone`.
- **Channels.** The stream-, sink- and fold-backed variants of the 0.6 `Channel` are separate
  types: `Channel::from_stream(s)` → `StreamChannel::new(s)` / `s.into_channel()`;
  `Channel::from_fold(init, Arc<step>)` → `FoldChannel::from_fold(init, step)`;
  `Channel::from_sink(sink)` → `FoldChannel::from_sink(sink)` (with `write`, `read`,
  `fold_state`, `consume_stream`, `map_out`, `flat_map_out`, `shutdown`). `channel.to_stream()`
  and `channel.to_sink()` work on the queue-backed `Channel`.
- **`Sink<A, In, E = Never, R = HNil>`**: `run(&self, stream)`; predicates are any
  `Fn(&In) -> bool`; new `Sink::new(driver)` and `Sink::for_each(f)`. `to_queue` suspends on a full
  bounded queue (0.6 dropped the item).
- **`broadcast`** uses per-branch sliding queues instead of a `PubSub` + `Scope`, and the pump
  yields after each element. `broadcast_with_replay(s, cap, replay, n)` keeps `max(cap, replay)`
  items per branch.
- **Throughput.** `with_throughput_metric(Metric)` is `with_throughput_counter(Arc<AtomicU64>)`.
- `Chunk`, `Transducer` (and `transducer_map` / `filter` / `take`), the windows, the joins,
  `state_scan` and `scan` keep their 0.6 behaviour.

## 11. Schedule, clock and random

### `Schedule<Out, In>` and `Effect::retry` / `Effect::repeat`

The untyped, stateful 0.6 `Schedule` enum (`fn next(&mut self, ..)`) and the free `retry*` /
`repeat*` functions are replaced by an immutable, typed `Schedule<Out, In>` (Effect.ts
`Schedule`) and associated functions on `Effect`:

```rust
// 0.6
retry(|| fetch(id), Schedule::exponential(Duration::from_millis(10)).compose(Schedule::recurs(3)))
repeat(|| poll(), Schedule::spaced(Duration::from_secs(1)))

// 0.7
use std::time::Duration;
use id_effect::{Effect, Schedule};
Effect::retry(move || fetch(id), Schedule::exponential(Duration::from_millis(10)).intersect(Schedule::recurs(3)))
Effect::retry_or_else(make, schedule, |last_err, last_out| fallback(last_err))
Effect::repeat(|| poll(), Schedule::spaced(Duration::from_secs(1)).intersect(Schedule::recurs(9)))
Effect::repeat_or_else(make, schedule, |err, last_out: Option<_>| recover(err))
```

- `retry` / `repeat` take the effect's **factory** (`Fn() -> Effect`), called once per attempt
  when the attempt starts, because an `Effect` value runs once (§3.4). Only typed failures are
  retried; defects and interrupts pass through.
- A `Schedule` holds no per-run state: `start()` makes a fresh `ScheduleState`, and
  `step(now, &input, state)` returns the next state, an output and the delay (`None` = done). One
  schedule value can drive many runs; clone it freely (one `Arc`).
- `In` is what the schedule sees: the error for `retry`, the value for `repeat`
  (`.while_input(|e| e.is_transient())`). `Out` is its output: `recurs` / `spaced` / `forever`
  count recurrences, `exponential` / `fibonacci` output the delay, `union` / `intersect` pair both
  outputs, and `map` transforms it.
- Combinators: `recurs`, `forever`, `spaced`, `exponential`, `exponential_by(base, factor)`,
  `fibonacci`, `jittered` (`[0.8, 1.2]`), `jittered_with(min, max, JitterSource)`, `union` (either
  continues, shorter delay), `intersect` (both continue, longer delay; **replaces `compose`**),
  `and_then`, `while_input`, `up_to`, `map`, `from_fn`.
- `recurs(n)` allows `n` recurrences, so `n + 1` attempts. `exponential(10ms)` intersected with
  `recurs(3)` sleeps 10 + 20 + 40 = 70 ms in total.
- Delays are `std::time::Duration`, slept on the fiber's executor clock (virtual on
  `TestExecutor`). The `*_with_clock` / `*_with_clock_and_interrupt` variants, `ScheduleDecision`
  and `ScheduleInput` are gone: the clock is the executor's and interruption is the fiber's.

### Clock

`Clock`, `LiveClock` and `TestClock` are removed. Time is the executor clock: `sleep(d)`, timeouts,
schedule delays, pool TTLs and metric durations read it, and `TestExecutor::advance(d)` moves it
in tests. `id_effect::Duration` and `UtcDateTime` (kernel-neutral, `scheduling::{duration,
datetime}`) are unchanged; public APIs take `std::time::Duration`.

### `Random` and `TestRandom` (new)

`Random` is an ordinary capability (xoshiro256**, no `rand` dependency): name it in `R` and read
it with `need` or the accessors `random_u64`, `random_f64`, `random_range` and `random_shuffle`.
Provide `Random::live()` in production and a seeded `Random::seeded(n)` / `TestRandom` in tests:

```rust
use id_effect::{Effect, Exit, HCons, HNil, Random, random_range};

fn roll() -> Effect<u64, (), HCons<Random, HNil>> {
  random_range(1..7)
}
let Exit::Success(n) = roll().provide(Random::seeded(42)).block_on() else { unreachable!() };
```

`TestRandom::seeded(seed)` gives a reproducible sequence, `feed_u64s` / `feed_f64s` queue exact
values, and `test_random.random()` is the `Random` to provide. Code that is not an effect takes a
`&dyn RandomSource`. `Schedule::jittered_with(.., JitterSource::from_fn(..))` can draw from it.

## 12. Observability

### Metrics and `MetricRegistry`

The 0.6 `Metric<In, Out>` (state behind `Arc<Mutex<u64>>` / `Mutex<Vec<Duration>>`, unbounded
sample vectors, no registry) is replaced by Effect.ts-style metrics on lock-free state, a keyed
`MetricRegistry`, fiber metric tags and span hooks:

```rust
// 0.6
let m = Metric::counter("requests", vec![("route".into(), "/a".into())]);
run_blocking(m.apply(1), ())?;
let h = Metric::histogram("latency", Vec::new());       // unbounded Vec<Duration>
let eff = h.track_duration(handler());

// 0.7
use id_effect::{Boundaries, Counter, Histogram, Metric, MetricRegistry};
let requests = Counter::new("requests").tagged("route", "/a");   // global registry
let eff = requests.increment().then(handler());                  // or requests.track_all(1, handler())
let latency = Histogram::new("latency", Boundaries::exponential(0.001, 2.0, 12));
let eff = latency.track_duration(handler()).tag_metrics("service", "api");
let pairs = MetricRegistry::global().snapshot();                 // keys with tags + states
```

- Kinds: `Counter` (`u64`, saturating), `Gauge` (`i64`, `set` / `adjust`), `Histogram` (fixed
  `Boundaries`, `boundaries + 1` buckets, the last `+inf`), `Summary`
  (`SummaryConfig::new(max_age, max_size, quantiles)`: a bounded window) and `Frequency` (counts per
  string). State is atomic; the summary window is the only lock. Histogram bucket counts are
  per bucket (not cumulative), and `NaN` samples are dropped. There is no `f64` counter or gauge.
- A metric is a `MetricKey` (name, `MetricKind`, sorted tags) in a registry; the same key is the
  same series, and `tagged(k, v)` is a different series. `MetricRegistry::new()` gives an isolated
  registry (for tests).
- Updates are effects through the `Metric` trait (`update`, `track_all`, `track`,
  `track_success_with`, `track_error_with`, `track_exit_with`, `track_duration_with`), or
  `update_now` outside an effect. Durations are recorded in seconds on the executor clock. 0.6's
  `make(..)` / `apply(..)` / `metric_make` have no counterpart.
- `eff.tag_metrics(k, v)` (a `FiberRef`, inherited by forked children) tags every metric update
  inside.
- Exporters: `MetricRegistry::snapshot()` (pull) and a `MetricListener` added with
  `add_listener` (push). `id_effect_opentelemetry` builds on both ([§14.13](#1413-id_effect_opentelemetry)).

### Spans and span hooks

`with_span(eff, name)` (a free function) becomes `eff.with_span(name)` / `with_span_attrs`, and
`Span::current()` reads the current span. A span is exported only through a **span hook**: an
exporter implements `SpanHook` and is installed with `eff.with_span_hook(hook)` (a `FiberRef`,
inherited by forked children; no thread-locals).

### The in-process tracing buffer

The 0.6 tracing buffer (`install_tracing_layer`, `snapshot_tracing`, `EffectEvent`, …) lives in
`observability::tracing` (re-exported at the root) and is fed by the `tracing_hook()` span hook:

```rust
// 0.6
use id_effect::{TracingConfig, install_tracing_layer, run_blocking, snapshot_tracing, with_span};
run_blocking(install_tracing_layer(TracingConfig::enabled()), ()).unwrap();
run_blocking(with_span(program, "request"), env);
let snap = snapshot_tracing();

// 0.7
use id_effect::{TracingConfig, install_tracing_layer, snapshot_tracing, tracing_hook};
install_tracing_layer(TracingConfig::enabled()); // a plain function now
let exit = program.with_span("request").with_span_hook(tracing_hook()).block_on();
let snap = snapshot_tracing();
```

- `install_tracing_layer` is a synchronous function: it turns recording on and clears the buffer.
- Spans are recorded only under `tracing_hook()` (`Effect::with_span_hook` /
  `LocalEffect::with_span_hook`, inherited by forked children). A span's attributes
  (`with_span_attrs`) seed its `SpanRecord` annotations. A defect or an interrupt ends a span with
  `EffectEvent::Failure`, like a typed failure.
- `annotate_current_span(key, value)` is an `Effect<(), E, R>` for any `E` / `R` (no turbofish, no
  sealed success/error traits; `AnnotateCurrentSpanSuccess` / `AnnotateCurrentSpanErr` and
  `TracingFiberRefs` are removed). `emit_effect_event` / `emit_fiber_event` / `emit_compute_event`
  are effect steps, and `record_*` are their synchronous forms.

## 13. Schema

Additive only. `tuple5`..`tuple12` and `struct5`..`struct12` follow the contract of the 4-ary
combinators (0.6 stopped at 4, so wider shapes had to nest):

```rust
use id_effect::schema::{bool_, i64, string, struct5};
let user = struct5(
  "id", i64::<()>(), "name", string(), "admin", bool_(), "age", i64(), "nick", string(),
);
```

The wire shape is unchanged: `tupleN` decodes an `Unknown::Array` of length `N` and prefixes
errors with the position, and `structN` decodes an `Unknown::Object`, prefixes errors with the
field name, and reports a missing key as `missing field <name>`. `#[derive(Schema)]` now compiles
for `i32` fields, `Option<bool>` / `Option<f64>` / `Option<i32>`, `Vec<i32>`, and `Vec<T>` where
`T: HasSchema` (the generated code used to fail with E0229). Every combinator round-trips
`decode(encode(a)) == a`.

## 14. Satellite crates

Every satellite crate is on the 0.7 kernel. Its effects are `id_effect::Effect`s, its providers
are `Layer`s, and its capabilities live in the typed `R`. The sections below list what changed
beyond that.

### 14.1 `id_effect_tokio`

`id_effect_tokio` is a thin layer over the kernel's `TokioExecutor` and `Runtime`. Every fiber is
an ordinary tokio task with a real waker; the 0.6 driver that ran each forked effect with
`run_blocking` on tokio's blocking pool (a busy-polling OS thread per fiber) is gone.

| 0.6 | 0.7 |
|---|---|
| `TokioRuntime: id_effect::Runtime` (the 0.6 trait) | `TokioRuntime` wraps an `id_effect::Runtime` (`runtime()`, `AsRef<Runtime>`, `From<TokioRuntime> for Runtime`) |
| `run_fork(&rt, \|\| (eff, env))` → `FiberHandle` | `rt.fork(eff)` → `FiberHandle` (`join_future().await`, `join()`, `interrupt_now()`) |
| `run_async(eff, env).await` | `rt.run(eff).await` → `Exit<A, E>` (provide capabilities first: `eff.provide_all(hlist![..])`) |
| `spawn_blocking_run_async(&handle, f)` / `rt.spawn_blocking_run_async(f)` | removed; `rt.spawn(eff)` → `tokio::task::JoinHandle<Exit<A, E>>` (effects are `Send`) |
| `rt.sleep(d)`, `id_effect_tokio::yield_now(&rt)` | `id_effect::sleep(d)`, `id_effect::yield_fiber()` (they use the fiber's executor) |
| `TokioRuntime::with_fabric`, `rt.fabric()`; constructors installed a default `ComputeFabric` | removed; admission is the ambient `Fabric` ([§6](#6-concurrency)) |
| — | `TokioExecutor` re-exported; `rt.executor()` |

Unchanged: `TokioRuntime::{current, from_handle, new_current_thread, new_multi_thread, handle,
now, block_on}` (`block_on` still needs an owned runtime and panics otherwise). The owned runtimes
now enable all compiled tokio drivers, and an owned runtime is shut down with
`shutdown_background` when the last clone drops, so dropping one inside an async context no
longer panics.

```rust
// 0.6
use id_effect::{Runtime, kernel::succeed, run_fork};
use id_effect_tokio::TokioRuntime;
let rt = TokioRuntime::new_current_thread()?;
let out = rt.block_on(async { run_fork(&rt, || (succeed::<u8, (), ()>(7), ())).join().await });
assert_eq!(out, Ok(7));

// 0.7
use id_effect::{Exit, HNil, succeed};
use id_effect_tokio::TokioRuntime;
let rt = TokioRuntime::new_current_thread()?;
let out = rt.block_on(rt.fork(succeed::<u8, (), HNil>(7)).join_future());
assert!(matches!(out, Exit::Success(7)));
```

### 14.2 `id_effect_cli`

`run_main` is the CLI program edge. It takes an effect (an `Effect` or a `LocalEffect`) plus the
environment for its `R`, runs it with `block_on` on `Runtime::global()`, and maps the whole `Exit`
to the exit code. Pass `HNil` when the effect needs nothing:

```rust
// 0.6
use id_effect::{Effect, succeed};
let eff: Effect<(), String, ()> = succeed(());
run_main(eff, (), RunMainConfig::minimal())

// 0.7
use id_effect::{Effect, HNil, succeed};
let eff: Effect<(), String> = succeed(());
run_main(eff, HNil, RunMainConfig::minimal())
```

A program over capabilities passes its typed environment (an `HCons` list) as `env`;
`examples/cli-minimal` runs an `Effect<(), ConfigError, ConfigEnv>` with its `ConfigEnv`.

- `run_main` maps the full `Exit`: a typed failure exits `1` and prints `error: {e:?}`, a defect (a
  `die` or a panic inside the effect) exits `101` and prints `error: defect: …`, and an interrupt
  exits `130` and prints `error: interrupted (…)`. In 0.6 a panic unwound out of `main`, and an
  interrupt was not observable.
- Called inside an async context (a tokio task), `run_main` runs nothing and exits `101` (the
  `block_on` refusal is a defect). Call it from `main`.
- The env bound is `R: Send + Sync + 'static`, the capability list.
- `exit_code_for_exit`, `exit_code_for_cause`, `cause_max_exit_byte` and `exit_code_for_result` are
  unchanged. `Exit` and `Cause` are the shared root types `id_effect::{Exit, Cause}`.
- `RunMainConfig::with_tracing()` installs the in-process tracing buffer and runs the program under
  `tracing_hook()`, so its `with_span`s show up in `snapshot_tracing()`.
- **The `id-effect-diagnose` binary is removed.** It printed diagnostics for the 0.6 runtime
  capability graph (`CapabilityGraph`, `ProviderSpec`, `plan_topological`). In 0.7 a missing or
  cyclic capability is a compile error, so there is nothing left to check. `id_effect_cli` no
  longer depends on `serde`, `serde_json` or `toml`.

### 14.3 `id_effect_config`

The `Config<T>` descriptor is the only way to read configuration (Effect.ts `Config`). The free
loaders and free combinators are gone, `Config::all` accumulates errors, a descriptor can be
turned into a layer, and every provider installs itself as the capability.

| 0.6 | 0.7 |
|---|---|
| `config::string(&p, "K")`, `config::integer`, `number`, `boolean`, `optional_string`, `string_list` | `Config::string("K").load(&p)` (`Config::integer`, …) |
| `config::nested_string(&p, "NS", "K")` (every `nested_*`) | `Config::string("K").nested("NS").load(&p)` or `Config::string("NS.K")` |
| `config::with_default(r, d)` | `Config::…(..).with_default(d)` |
| `config::all(a, b)`, `all3(a, b, c)`, `all_vec(v)` | `Config::all((a, b))`, `Config::all((a, b, c))` (tuples up to 12), `Config::all(vec)`, `Config::all([..; N])` |
| `zip_with(a, b, f)` | `a.zip_with(b, f)` (and `a.zip(b)`) |
| `or_else_config(a, b)` / `config::or_else(a, b)` | `a.or_else(b)` |
| `nest("NS", c)` | `c.nested("NS")` |
| `read_string(&["K"])`, `read_i64`, `read_bool`, `read_number`, `read_string_opt`, `read_string_list`, `read_nested_*`, `nested_path` | `Config::string("K").run()` (`Config::integer(..).run()`, …; nesting with `.nested(..)`) |
| `WithConfigDefault::with_default` on an effect | `Config::with_default` on the descriptor, before `.run()` |
| the `id_effect_config::config` module | import `Config` from the crate root |
| `Config::run::<A, E, R>()` with `R: Needs<ConfigProviderService>` | `Config::run::<A, E, R, I>()` with `R: Selector<ConfigProviderService, I>` (`I` inferred) |
| `ConfigEnv = id_effect::Env` | `ConfigEnv = HCons<ConfigProviderService, HNil>` |
| `provide_config_provider(p) -> ProviderBox` (and `provide_env_config_provider(opts)`, `provide_figment_config_provider(fig)`) | the same names → `Layer<ConfigEnv, E>` |
| `provide!(EnvConfigProviderLive)` | `EnvConfigProviderLive::layer()` |
| `Env::scoped([provide_config_provider(..)])` | `.provide_layer(provide_config_provider(..))` on the inner effect |

Behaviour changes:

- **`Config::all` accumulates** (Effect.ts `Config.all`): every component is loaded, and two or
  more failures come back as one `ConfigError::And(Vec<ConfigError>)` (flat, in component order);
  one failure is returned as is. `ConfigError::errors()` lists the individual failures. 0.6 `all`
  stopped at the first failure. `ConfigError` is `#[non_exhaustive]`, so a `match` that renders
  variants needs an arm (or `_`) for `And`.
- `with_default` and `or_else` fall back only when **every** failure is a missing key
  (`ConfigError::is_missing_only`, Effect.ts `isMissingDataOnly`), so they also apply to a
  composite `Config::all` whose keys are all absent. An invalid value still propagates.

New: `Config::<T>::layer::<E>() -> Layer<HCons<T, HNil>, E, ConfigEnv>` loads the descriptor when
the layer is built and provides the value as a capability (`E: From<ConfigError>`; a load failure
fails the build). `ConfigProvider::layer::<E>()` exists on every provider (Effect.ts
`Layer.setConfigProvider`); `ConfigProviderService::new(p)` wraps a provider, and the service
itself implements `ConfigProvider`. The providers, `Secret<T>`, `ConfigError` and the Figment
helpers are otherwise unchanged.

```rust
// 0.6
let p = EnvConfigProvider::from_env();
let host = config::nested_string(&p, "SERVER", "HOST")?;
let port = config::with_default(config::nested_integer(&p, "SERVER", "PORT"), 8080)?;

// 0.7: synchronous
use id_effect_config::{Config, EnvConfigProvider};
let (host, port) = Config::all((Config::string("HOST"), Config::integer("PORT").with_default(8080)))
  .nested("SERVER")
  .load(&EnvConfigProvider::from_env())?;

// 0.7: the loaded value as a capability
struct Server { host: String, port: i64 }
let server = Config::all((Config::string("HOST"), Config::integer("PORT").with_default(8080)))
  .nested("SERVER")
  .map(|(host, port)| Server { host, port })
  .layer::<AppError>();
let app = EnvConfigProvider::from_env().layer().provide_to(server);
program.provide_layer(app) // program: Effect<_, AppError, HCons<Server, HNil>>

// 0.7: read inside an effect
let program = Config::integer("PORT")
  .run::<i64, ConfigError, ConfigEnv, _>()
  .provide_layer(provide_config_provider(MapConfigProvider::from_pairs([("PORT", "8080")])));
```

### 14.4 `id_effect_logger`

The thread-local install API is unchanged (`install_*`). The backends (`LogBackend`, `Logger`,
`CompositeLogBackend`, `JsonLogBackend`, `StructuredLogBackend`, `TracingLogBackend`) are
untouched. The effects use the 0.7 kernel types, and the providers keep their names but are
`Layer`s:

| 0.6 | 0.7 |
|---|---|
| `EffectLogger::{log, log_string, trace, debug, info, warn, error, fatal}` → `Effect<(), EffectLoggerError>` with `R = ()` | the same with `R = HNil` |
| `annotate_logs`, `with_log_span` (`R: Send`) | `R: Send + Sync` |
| `EffectLogMinLevel` = the 0.6 `FiberRef<LogLevel>` | the 0.7 `FiberRef<LogLevel>` |
| `provide!(EffectLoggerLive)` | `EffectLoggerLive::layer()` → `Layer<HCons<EffectLogger, HNil>, E>` |
| `provide!(LogMetadataLive)` | `LogMetadataLive::layer()` → `Layer<HCons<EffectLogMetadata, HNil>, E>` |
| `provide_composite_logger(c) -> ProviderBox` | `provide_composite_logger(c)` → `Layer<HCons<EffectLogComposite, HNil>, E>` |
| `provide_minimum_log_level(l) -> ProviderBox` | `provide_minimum_log_level(l)` → `Layer<HCons<EffectLogMinLevel, HNil>, E>` |
| `Needs::<EffectLogMinLevel>::need(&env).clone()` | the `FiberRef` in the built layer's output |
| `EffectLoggerLive`, `LogMetadataLive` derive `ProviderSpec` | plain `Copy` unit structs with a `layer()` |
| `.into_env()` on a log step | `.narrow()` |

```rust
// 0.6
let env = build_env([
  provide!(LogMetadataLive),
  provide_composite_logger(comp),
  provide_minimum_log_level(LogLevel::Info),
])?;
assert_eq!(run_with([provide!(EffectLoggerLive)], program()).unwrap(), 7);

// 0.7
use id_effect::{Exit, HCons, HNil, Never};
let pipeline = LogMetadataLive::layer()
  .merge(provide_composite_logger(comp))
  .merge(provide_minimum_log_level(LogLevel::Info));
let exit = app().provide_layer(pipeline).block_on(); // app: Effect<_, E, caps!(EffectLogMetadata, ..)>
assert_eq!(program().provide(EffectLogger).block_on(), Exit::Success(7));
assert_eq!(EffectLogger.warn("plain value").block_on(), Exit::Success(()));

// or build up front, keeping the minimum-level ref
let HCons(fr, HNil) = provide_minimum_log_level::<Never>(LogLevel::Info)
  .build()
  .block_on()
  .into_result()
  .expect("infallible");
```

- **The layers install their thread-local state when they are built**, on the thread that drives
  the build. Each `provide_layer` run installs a fresh minimum-level ref (and fresh annotation and
  span refs), like rebuilding the 0.6 providers did.
- **The minimum level is a `FiberRef`.** Its value lives in the running fiber, so `fr.set(level)`
  affects the rest of the same program (and fibers it forks afterwards), not a later `block_on`:
  each run edge starts a root fiber from the ref's initial value (the level given to
  `provide_minimum_log_level`). In 0.6 a `run_blocking(fr.set(..), ())` call leaked into later
  runs on the thread. Set the level inside the program, or scope it with
  `EffectLogger::with_minimum_log_level(fr, level, inner)`.
- **Thread-locals are read on the thread that runs the step.** The blocking edge
  (`Effect::block_on`) drives the root fiber on the calling thread, so a program run there sees
  what that thread installed. Fibers that run on executor worker threads (`fork`, `Runtime::run`
  on tokio) see only what those threads installed; without an installed composite backend they
  fall back to `tracing`.
- Annotations and the span stack follow `FiberRef` inheritance: a forked child starts with a copy
  of its parent's annotations and spans.

### 14.5 `id_effect_optics`

No change for users. The crate does not use the effect API. `id_effect_optics::transducer::{Reducer,
Transducer, filter, map, take}` re-exports `id_effect::streaming::transducer`, the same type that
`Stream::transduce_items` uses, with the same names and signatures.

### 14.6 `id_effect_fp`

The pure parts (`algebra`, `collections`, `foundation`, `result`, `reader`, `thunk`) are
unchanged. The items that produce or take an effect use `id_effect::Effect`:

| item | 0.6 | 0.7 |
|---|---|---|
| `stm::commit`, `stm::atomically` | a synchronous body, `R: 'static` | an async step, `R: Send + Sync` |
| `traverse_vec`, `sequence_vec`, `traverse_option` | `F: FnMut(A) -> Effect<..> + 'static` | `A, B, E: Send`, `R: Send + Sync`, `F: Send` |
| `select_effect`, `branch_effect` | the 0.6 `Effect` | `Effect`; values and handler closures `Send` |
| `FreeAp<A, E, R>` | over the 0.6 `Effect` | over `Effect`; its `Ap2` closure is `Send` |
| `bifoldable::bitraverse::bitraverse` | the 0.6 `Effect` | `Effect` |

```rust
// 0.6
use id_effect::run_blocking;
let cell = run_blocking(commit(TRef::make(0_i32)), ()).expect("cell");
run_blocking(commit(cell.update_stm::<(), _>(|n| n + 1)), ()).expect("update");
assert_eq!(run_blocking(commit(cell.read_stm::<()>()), ()), Ok(1));

// 0.7
use id_effect::Exit;
let cell = commit(TRef::make(0_i32)).block_on().into_result().expect("cell");
commit(cell.update_stm::<(), _>(|n| n + 1)).block_on().into_result().expect("update");
assert_eq!(commit(cell.read_stm::<()>()).block_on(), Exit::succeed(1));
```

- **`R = ()` becomes `R = HNil`**, the default, so `commit::<_, _, ()>(..)` turbofishes go away.
- **Retry parks instead of spinning** (Effect.ts `STM.retry`). In 0.6 a retrying transaction looped
  on `std::thread::yield_now`, burning a thread. Now `commit` suspends the fiber and registers a
  waker on every `TRef` the attempt read or wrote; the next commit that writes one of them wakes
  it. A retry on a single-threaded executor (such as `TestExecutor`) makes progress instead of
  hanging.
- **Interruption.** A parked transaction is interruptible, and interrupting it removes its waker
  from every cell. A transaction that retries without touching any `TRef` (a bare `Stm::retry()`)
  waits until it is interrupted.
- **Conflicts restart at once** (no yield).
- **`Stm::or_else` rolls back the retrying branch's writes** before it runs the alternative, so the
  alternative sees the values from before the first branch (Effect.ts `orTry`). In 0.6 those
  writes leaked into the alternative and could be committed with it.
- **A typed failure from an inconsistent snapshot restarts the attempt.** `Stm::fail` ends the
  commit only when every cell the attempt read is still at the version it saw.

### 14.7 `id_effect_resilience`

Every operation returns an `Effect`, generic over its environment `R`. Constructors are plain
functions (they were effects), and all time (breaker cooldown, token refill, retry backoff, hedge
delay) is read from the running fiber's executor clock, so it is virtual time on `TestExecutor`.
The `*_with_clock` variants are gone.

| item | 0.6 | 0.7 |
|---|---|---|
| `CircuitBreaker` | `CircuitBreaker::make(n, reset)` (an effect), `make_with_clock(.., clock)` | `CircuitBreaker::new(n, reset)` |
| `CircuitBreaker::call` / `state` | `Effect<A, Or<CircuitBreakerError, E>, R>` / `Effect<CircuitState>` | the same / `Effect<CircuitState, E, R>` |
| `RateLimiter` | `RateLimiter::make(rate, cap)` (an effect), `make_with_clock` | `RateLimiter::new(rate, cap)`; `acquire::<R>()` |
| `Bulkhead` | `Bulkhead::make(n)` (an effect) | `Bulkhead::new(n)`, on `Semaphore`; `available()` added |
| `retry_with_breaker` | `(FnMut() -> Effect, 0.6 Schedule, &breaker)`; `retry_with_breaker_and_clock` | `(Fn() -> Effect + Send + Sync, Schedule<Out, E>, &breaker)`, built on `Effect::retry`; the `_and_clock` variant is gone |
| `hedged` | `tokio::select!`, `R = ()` only: the first *completion* wins | `primary.race(sleep(delay).then(backup))`, any `R`: the first *success* wins |

```rust
// 0.6
let breaker = run_blocking(CircuitBreaker::make(3, Duration::from_secs(30)), ()).unwrap();
let out = run_blocking(
  retry_with_breaker(|| call(), Schedule::exponential(ms(100)).compose(Schedule::recurs(5)), &breaker),
  (),
);
let bh = run_blocking(Bulkhead::make(8), ()).unwrap();

// 0.7
use std::time::Duration;
use id_effect::Schedule;
use id_effect_resilience::{Bulkhead, CircuitBreaker, retry_with_breaker};
let breaker = CircuitBreaker::new(3, Duration::from_secs(30));
let out = retry_with_breaker(
  || call(), // a factory: effects are one-shot, so each attempt builds a fresh one
  Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(5)),
  &breaker,
)
.block_on();
let bh = Bulkhead::new(8);
```

- **Tests on virtual time.** Replace `TestClock` + `make_with_clock` with a program that sleeps on
  a `TestExecutor`: `cb.call(fail(..)).then(sleep(reset)).then(cb.call(probe))` sees the breaker
  half-open after `reset` of virtual time, and backoff delays advance the same clock.
- **`hedged` semantics changed.** It is `Effect::race`: a primary that *fails* before the delay no
  longer ends the hedge; the backup still starts at the delay and decides. The loser is
  interrupted and awaited. When both fail, the cause is `Cause::Both(primary, backup)`. The backup
  never starts if the primary succeeds first.
- **Defects and interrupts** pass through `CircuitBreaker::call` without being counted and are
  never retried by `retry_with_breaker`. `Bulkhead::with_permit` releases its permit however the
  effect ends, interrupts included.
- `RateLimiter` counts refill from its first `acquire` (the bucket starts full).

### 14.8 `id_effect_sql`

Every trait method returns an `Effect<_, SqlError>` (no `R`), the capabilities live in the typed
environment, and the providers are `Layer`s. The client and transactions are **scoped layers**
(Effect.ts `Layer.scoped`): their resources are released when the layer's scope closes, after the
program, however it ends.

| 0.6 | 0.7 |
|---|---|
| `SqlClient` methods → `Effect<_, SqlError, ()>` | → `Effect<_, SqlError>`; new `close()` (default: no-op) |
| `SqlTransaction { commit, rollback }` | adds `query` and `execute`: **statements on the handle run inside the transaction** (on pg, on the transaction's connection; in 0.6 `PgSqlClient` statements always went to the pool, outside the transaction) |
| `SqlClientService(Arc<dyn SqlClient>)` in an `Env` | `SqlClientService` (derefs to `dyn SqlClient`, `SqlClientService::new(c)`) in `SqlClientEnv = HCons<SqlClientService, HNil>`; `sql_client_env(c)` for `.provide_all` |
| — | `sql_client_layer::<E>(client)`: connects on build (a failure fails the build with `E::from`), `close` when the scope closes |
| — | `provide_sql_client::<E>(client)`: `Layer::succeed`, owned by the caller |
| — | `query(sql, params)` / `execute(sql, params)` read the client from `R` |
| `with_transaction(client, f)` / `transaction_scope(client, f)`, rollback on failure only | → `Effect<A, E>` with `E: From<SqlError>`; commits on success and rolls back on failure, **defect, panic or interrupt** (`acquire_use_release`); `BEGIN` and `COMMIT` run uninterruptibly; a failed `COMMIT` is a typed error. `transaction_scope` stays as an alias |
| — | `transaction(f)`: the same bracket, reading the client from `R` |
| — | `transaction_layer::<E>()`: `Layer<SqlTransactionEnv, E, SqlClientEnv>`: `BEGIN` on build; on scope close `COMMIT` when the owner succeeded, `ROLLBACK` on failure, defect or interrupt. A failed `COMMIT` there is a **defect** of the owner (a release cannot fail with a typed error); use the brackets when the commit error must be typed |
| `pg::PgPool = Arc<sqlx::PgPool>`, `provide_pg_pool(pool) -> ProviderBox` | `pg::PgPool(pub sqlx::PgPool)` (derefs to `sqlx::PgPool`), `PgPoolEnv`; `provide_pg_pool::<E>(pool) -> Layer<PgPoolEnv, E>` |
| `provide_pg_sql_client(pool) -> ProviderBox` | `provide_pg_sql_client::<E>(pool) -> Layer<PgSqlEnv, E>`, `PgSqlEnv = HCons<SqlClientService, HCons<PgPool, HNil>>` (the caller owns the pool) |
| — | `pg::PgSqlClient::layer::<E>(PgPoolConfig)`: opens the pool and checks it (`SELECT 1`) on build, **closes the pool** when the scope closes |
| `PgSqlTransaction` over a `PoolConnection` + a manual `BEGIN` | over a sqlx `Transaction` (`pool.begin()`): a handle dropped unfinished is rolled back by sqlx |
| `TestSqlClient` rollback cleared every recorded statement | a transaction's statements are recorded when it commits and discarded when it rolls back; new `commits()`, `rollbacks()`, `closes()`, `is_connected()`, `recorded_executes()`, `fail_commits()` |
| `examples/010_axum_sql.rs` (a dev-dependency on `id_effect_axum`) | removed; the axum + SQL wiring lives in the `examples/http-showcase` workspace example |

Unchanged: `SqlError`, `SqlParam`, `SqlRow`, `PgSqlClient::{new, pool}`, `PgPoolConfig`,
`pg_pool_from_config`, `pg_pool_from_config_lazy`, `PgSqlError`. The live implementations need a
tokio runtime with its IO driver (sqlx): run them with `id_effect_tokio::TokioRuntime`. The test
double runs anywhere, `block_on` included.

```rust
// 0.6
let pool = pg_pool_from_config(PgPoolConfig::from_url(url)).await?;
let db: Arc<dyn SqlClient> = Arc::new(PgSqlClient::new(pool.clone()));
let n = run_async(with_transaction(db, |tx| /* statements on the pool, not on tx */ ..), ()).await?;

// 0.7
use id_effect::{Effect, need};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient};
use id_effect_sql::{SqlParam, SqlTransactionEnv, SqlTransactionService, transaction, transaction_layer};
let insert: Effect<u64, AppError, SqlTransactionEnv> = need::<SqlTransactionService>().flat_map(|tx| {
  tx.execute("INSERT INTO todos (title) VALUES ($1)", &[SqlParam::Text(t)])
    .map_err(AppError::from)
    .narrow()
});
let program = insert
  .provide_layer(transaction_layer())                              // BEGIN … COMMIT / ROLLBACK
  .provide_layer(PgSqlClient::layer(PgPoolConfig::from_url(url))); // open pool … close pool
let exit = TokioRuntime::current()?.run(program).await;

// or, as a bracket with a typed commit error:
let n = transaction(|tx| tx.execute(sql, &params).map_err(AppError::from).narrow());
```

### 14.9 `id_effect_process`

Every process is a fiber on an `id_effect::Runtime`, and every behaviour callback returns an
`Effect`. The 0.6 thread-per-process node, the busy-polling `SpinClock` and the mailbox effects
interpreted in place with `run_blocking` are gone. Time comes from the runtime's executor clock,
so a node on a `TestExecutor` runs call timeouts, shutdown deadlines and restart-intensity windows
on virtual time.

| 0.6 | 0.7 |
|---|---|
| `ProcessNode::threads(name)` | `ProcessNode::new(name)` (fibers on `Runtime::global()`) |
| `ProcessNode::on_runtime(name, runtime)` | `ProcessNode::on_runtime(name, Runtime)` / `ProcessNode::on_executor(name, Arc<dyn Executor>)` |
| `ProcessNode::with_spawner(name, clock, spawner)`, `Job`, `Spawner` | removed (use `on_executor`, e.g. with a `TestExecutor`) |
| `SpinClock`, `ProcessNode::clock()` | removed; `ProcessNode::now()` is the executor clock, `node.runtime()` the runtime |
| `Process` callbacks return `Effect<_, _, Env>`; `type Env: Send` | the same with `Process: Send`, `type Env: Send + Sync` (provided once at spawn, `HNil` or any value) |
| a panic in a callback was caught with `catch_unwind` | it reaches the loop as a `Defect` (`Cause::Die`); the exit reason is still `Error("panic: <message>")` |
| `Addr::{call, cast, send}` → `Effect<_, _, ()>` | generic over the environment `R` (`Effect<_, _, R>`); run at the edge with `block_on` / `Runtime::run` |
| `Addr::clone_mailbox()` → `Queue<ProcMsg<P>>` | → `Mailbox<ProcMsg<P>>` (`offer_now`, `close`, `take`) |
| `ProcMsg::Call(_, Deferred)`, `ProcessCell::exited()` | the 0.7 `Deferred` |
| `ProcessCell::cancellation()` | removed (kills stay cooperative: the reason is recorded and the mailbox closed) |
| `SupervisorHandle::stop(..)` was an `async fn` | returns an effect (`Effect<(), E, R>`) |
| supervisor `Env = ()` | `SupervisorProcess` / `DynamicSupervisor` use `Env = HNil` |
| — | `ProcessNode::await_exit(&pid)`: waits until the process is fully finalized |

The exit-signal, link, monitor, registry and supervision semantics (ADR 0009) are unchanged.
Supervisor restarts stay immediate (OTP): the budget is the sliding `RestartIntensity` window,
not a backoff delay.

```rust
// 0.6
let node = ProcessNode::threads(NodeName::new("app@local"));
let tree = SupervisorSpec::new(Strategy::OneForOne).child(spec).start(&node);
let kids = run_blocking(tree.which_children(Duration::from_secs(5)), ())?;

// 0.7
use std::sync::Arc;
use id_effect::{HNil, TestExecutor};
let node = ProcessNode::new(NodeName::new("app@local"));
let tree = SupervisorSpec::new(Strategy::OneForOne).child(spec).start(&node);
let kids = tree.which_children::<HNil>(Duration::from_secs(5)).block_on().into_result()?;

// Deterministic test: virtual time.
let exec = TestExecutor::new();
let node = ProcessNode::on_executor(NodeName::new("t@test"), Arc::new(exec.clone()));
exec.run_until_stalled();
exec.advance(Duration::from_secs(5)); // fires shutdown deadlines, slides intensity windows
```

### 14.10 `id_effect_platform`

Every trait method and free function returns an `Effect`, the capabilities live in the typed
environment, and the providers are `Layer`s.

| capability | 0.6 | 0.7 capability | 0.7 layers |
|---|---|---|---|
| HTTP | `HttpClientService = Arc<dyn HttpClient>`, `provide_reqwest_http_client() -> ProviderBox`, `env_set_http_client`, `env_has_http_client` | `HttpClientService(pub Arc<dyn HttpClient>)` (derefs to `dyn HttpClient`), `HttpClientEnv` | `ReqwestHttpClientProvider::layer()` (also `provide_reqwest_http_client()`), `provide_http_client(client)`; `http_client_env(client)` for `.provide_all` |
| filesystem | `FileSystemService = Arc<dyn FileSystem>`, `provide!(LiveFileSystemProvider)` | `FileSystemService(pub Arc<dyn FileSystem>)`, `FileSystemEnv` | `LiveFileSystemProvider::layer()`, `provide_file_system(fs)`; `file_system_env(fs)` |
| command | `ProcessRuntimeService = Arc<dyn ProcessRuntime>`, `provide!(TokioProcessRuntimeProvider)` | `ProcessRuntimeService(pub Arc<dyn ProcessRuntime>)`, `ProcessRuntimeEnv` | `TokioProcessRuntimeProvider::layer()`, `provide_process_runtime(rt)`; `process_runtime_env(rt)` |
| reqwest client | `ReqwestClient`, `provide!(ReqwestClientLive)`, `provide_reqwest_client(c) -> ProviderBox` | `ReqwestClient` (unchanged), `ReqwestClientEnv` | `ReqwestClientLive::layer()`, `provide_reqwest_client(c)` |
| reqwest pool | `ReqwestPool` = a 0.6 `Pool<PooledClient, Never>`, `provide_reqwest_pool(n, ttl) -> ProviderBox` | `ReqwestPool = Pool<PooledClient>`, `ReqwestPoolEnv` | `provide_reqwest_pool(n, ttl)` (a new pool per build) |

Every layer is a `Layer<Env, E>` with `In = HNil` and a free `E`, so they merge into one platform
layer under the application's error type:

```rust
let platform: Layer<_, AppError> = LiveFileSystemProvider::layer()
  .merge(ReqwestHttpClientProvider::layer())
  .merge(TokioProcessRuntimeProvider::layer());
```

- `fs::{read, exists}`, `command::{spawn, spawn_wait}`, `http::{execute, execute_stream}` and the
  `http::reqwest` helpers (`send`, `text`, `bytes`, `json`, `json_schema`, `send_pooled`, `get`,
  `post`, `put`, `delete`, `head`, `patch`) take `R: Selector<Service, I> + HList` instead of
  `R: Needs<Service>`. The index `I` is inferred: one more `_` in a turbofish
  (`text::<String, Error, _, _>(..)` still has four, because `build` is now an `impl FnOnce`
  argument).
- When the effect is fed to `provide_layer`, name `R`: `execute::<HttpClientEnv, _>(req)`.
  `provide_all(http_client_env(..))` infers it.
- `child_wait::<R>` / `child_kill::<R>` take any `R: Send + Sync + 'static`.
  `ProcessRuntime::spawn_wait` is `spawn(..).flat_map(child_wait)`.
- **Streaming bodies.** `StreamingHttpResponse::body` was a `Stream<u8, HttpError, ()>` fed by a
  spawned task; it is now an `HttpBody`, read on demand (`next_chunk()`, `collect()`, both effects),
  with the `max_body_bytes` cap enforced as chunks arrive. Test doubles build one with
  `HttpBody::from_chunks`, `HttpBody::from_bytes(bytes, chunk_size)` or `HttpBody::empty()`.
  `body_as_chunk_stream(body, n)` is `HttpBody::from_bytes(&body, n)`, `response_body_stream(&resp,
  n)` returns an `HttpBody`, and `response_body_chunk(&resp)` is removed (use `resp.body`).
- **Runtime.** The live implementations need a tokio runtime **with its IO driver** (reqwest,
  `tokio::process`): `TokioRuntime::current()?.run(eff).await`, `Runtime::global()`, or
  `Runtime::from_executor(..)` over a `TokioExecutor` inside such a runtime. The in-memory doubles
  (`TestFileSystem`, `MemorySessionStore`, `MemoryOAuthClient`) run anywhere, `block_on` included.
- `auth` (`SessionStore`, `OAuthClient`, the memory doubles) is ported the same way: the methods
  return `Effect<_, _>`.

```rust
// 0.6
let env = build_env([provide_reqwest_http_client()])?;
let res = run_async(execute::<Env>(HttpRequest::get(url)), env).await?;

let mut env = Env::new();
env_set_http_client(&mut env, Arc::new(Canned));
let res = run_blocking(execute::<Env>(HttpRequest::get(url)), env)?;

// 0.7
use id_effect_platform::http::{HttpClientEnv, HttpRequest, ReqwestHttpClientProvider, execute, http_client_env};
let program = execute::<HttpClientEnv, _>(HttpRequest::get(url))
  .provide_layer(ReqwestHttpClientProvider::layer());
let exit = TokioRuntime::current()?.run(program).await;

let exit = execute(HttpRequest::get(url)).provide_all(http_client_env(Canned)).block_on();
```

### 14.11 `id_effect_fsm`

Transition actions, `Interpreter::{step, run}` and `Saga::{run, compensate_indices}` use
`Effect`. The environment `R` is the typed environment (an HList such as `HCons<Db, HNil>`, or
`HNil`) read with `need::<T>()`, not a `&mut R` value. Steps and compensations are sequenced with
`flat_map` / `catch` on the caller's fiber; nothing in the crate parks a thread. The pure parts
(`TransitionTable`, `StateMachine`, `Fsm` / `#[derive(Fsm)]`, `to_mermaid`, the matchers,
`FsmError`, `SagaError`) are unchanged.

| 0.6 | 0.7 |
|---|---|
| actions and saga steps: `Effect<A, Err, R>` over `&mut R` | `Effect<A, Err, R>`, `R: Send + Sync` (an HList; read capabilities with `need::<T>()`) |
| `Interpreter::step` / `run` → `Effect<StateMachine, RunError, R>` | the same; `S`, `E`, `A`, `Err` must be `Send` (`S`, `E` also `Sync`) |
| `Saga::run` / `compensate_indices` → `Effect<(), SagaError<Err>, R>` | the same. Only typed failures are compensated; a defect or an interrupt passes through without compensation (in 0.6 a panic was not compensated either) |
| run with `run_blocking(program, env)` | `program.provide(..)` / `.provide_env(..)`, then run at the edge or compose it into the caller's effect |
| `FsmProcess` (`Args = FsmSpec`, `Env = R`) | the same shape: the actions' environment is the process environment, provided once at spawn (`spawn_linked_to::<FsmProcess<..>>(sup, spec, env)`, `ChildSpec::worker::<FsmProcess<..>, _>(id, move \|\| (spec.clone(), env.clone()))`). An action now runs on the process fiber, so it can suspend (e.g. `sleep`) |
| `FsmProcess::start(node, spec, env)` / `start_linked(ctx, spec, env)` | unchanged signature; `env: R` with `R: Send + Sync` |
| `FsmHandle<S, E, Err, R>` with `R: Send` | `R: Send + Sync`; `FsmHandle::{state, fire, cast}` return effects |

```rust
// 0.6
use id_effect::{Effect, run_blocking};
let interp = Interpreter::<Door, DoorEvent, (), String, u32>::new()
  .on_transition(Door::Closed, DoorEvent::Lock, || {
    Effect::new(|locks: &mut u32| {
      *locks += 1;
      Ok(())
    })
  });
let door = run_blocking(interp.run(Door::state_machine(), [DoorEvent::Lock]), 0)?;

// 0.7
use std::sync::atomic::{AtomicU32, Ordering};
use id_effect::{HCons, HNil, need};
let interp = Interpreter::<Door, DoorEvent, (), String, HCons<AtomicU32, HNil>>::new()
  .on_transition(Door::Closed, DoorEvent::Lock, || {
    need::<AtomicU32>().map(|locks| {
      locks.fetch_add(1, Ordering::SeqCst);
    })
  });
let door = interp
  .run(Door::state_machine(), [DoorEvent::Lock])
  .provide(AtomicU32::new(0))
  .block_on() // program edge only
  .into_result()?;

// FsmProcess: R is an HList now
FsmProcess::start(&node, spec, HCons(audit_log, HNil)); // R = HCons<Mutex<Vec<_>>, HNil>
```

### 14.12 `id_effect_node`

Everything in the distribution layer that waits is an `Effect`: `Send`, interruptible, and timed
on the executor clock. The 0.6 sleep-polling waits (`Mesh::await_deferred` looped on a poll plus
`thread::sleep(2ms)`), the thread per inbound call, the thread per orphan retry, and the Postgres
lease's worker thread with its own tokio runtime are fibers now.

| 0.6 | 0.7 |
|---|---|
| `Mesh::remote_spawn(..) -> Result<Pid, RemoteFault>` (parks the caller) | `-> Effect<Pid, RemoteFault, R>`: sends when it runs; the `timeout` is on the calling fiber's executor clock; interruptible, never leaves a pending entry |
| `Mesh::remote_call(..) -> Result<Vec<u8>, RemoteFault>` | `-> Effect<Vec<u8>, RemoteFault, R>` (same contract) |
| `Mesh::remote_spawn_placed(..) -> Result<Pid, RemoteFault>` | `-> Effect<Pid, RemoteFault, R>` |
| an inbound `Call` was answered from a spawned OS thread | answered by a daemon fiber on the local `ProcessNode`'s runtime |
| `BehaviorRegistry::deliver_call(pid, bytes, Deferred)` | `deliver_call::<R>(pid, bytes) -> Result<Effect<Vec<u8>, RemoteFault, R>, RemoteFault>` (enqueued at once; the effect awaits the encoded reply); `CallWireFn` returns a `ReplyFuture`; `BehaviorRegistry::adopt_local` takes a `Mailbox` |
| `DistributedSupervisor::start_child(..) -> Result<Pid, RemoteFault>` | `-> Effect<Pid, RemoteFault, R>` (candidates tried in order, each spawn bounded) |
| `DistributedSupervisor::on_membership(&e) -> Option<RetryHandle>` | `-> Effect<Option<RetryHandle>, Never, R>`: `NodeDown` completes once the takeover is done; `NodeUp` / `Rejoined` fork the orphan retry as a daemon fiber |
| `RetryHandle = std::thread::JoinHandle<Vec<(String, RemoteFault)>>` | `RetryHandle = FiberHandle<Vec<(String, RemoteFault)>, Never>` (`join()` / `interrupt_now()`; an interrupted retry leaves its children orphaned and retryable) |
| `DistributedSupervisor::retry_orphaned() -> Vec<..>` | `-> Effect<Vec<..>, Never, R>` |
| `SingletonLease` methods return `Result<_, SingletonError>` / `Option<String>` | return `Effect<(), SingletonError>` / `Effect<Option<String>, Never>` |
| `ClusterSingleton::{try_become, heartbeat, resign}` → `Result`, `is_holder()` → `bool` | → `Effect<(), SingletonError>`, `is_holder()` → `Effect<bool, Never>` (`local_pid()` stays sync) |
| `PgAdvisoryLease::connect(url)` / `from_pool_config(cfg)` blocked on a worker thread | → `Effect<PgAdvisoryLease, SingletonError>`; the dedicated session is a daemon fiber, interrupted when the handle drops. Run it on a tokio runtime with the IO driver (`Runtime::new()` / `Runtime::global()` have it); elsewhere the connect fails with `Unavailable("lease worker exited before ready: …")` |

Unchanged: `remote_cast` / `remote_info` / `remote_monitor` / `remote_demonitor` / `peer_down`
(synchronous, at-most-once), `fanout_cast`, transports, sessions, the handshake, SWIM membership,
process groups, placement and the `control` DTOs. The wire protocol and its semantics (ADR 0009)
are unchanged, so 0.6 and 0.7 nodes interoperate on the wire.

```rust
// 0.6
let pid = mesh.remote_spawn("b@host", "counter", args, None, Duration::from_secs(2))?;
let reply = mesh.remote_call(&pid, msg, Duration::from_secs(2))?;
lease.try_acquire("leader", "a", ttl)?;

// 0.7: compose inside a program …
use id_effect::Effect;
let program: Effect<Vec<u8>, RemoteFault> = mesh
  .remote_spawn("b@host", "counter", args, None, Duration::from_secs(2))
  .flat_map(move |pid| mesh2.remote_call(&pid, msg, Duration::from_secs(2)));
// … or run at the edge.
let reply = program.block_on().into_result()?;
let lease = PgAdvisoryLease::connect(url).block_on().into_result()?;
lease.try_acquire("leader", "a", ttl).block_on().into_result()?;
```

### 14.13 `id_effect_opentelemetry`

The OTLP, config, propagation, subscriber, logs and shutdown APIs are unchanged. What changed is
how effects reach OpenTelemetry:

| item | 0.6 | 0.7 |
|---|---|---|
| metrics | `CounterBridge` / `DurationHistogramBridge`: dual-write one `Metric` and one OTEL instrument per call | `bridge_metrics(&registry, meter)` / `bridge_global_metrics(meter)`: every series of a `MetricRegistry` is exported |
| spans | `with_span_otel(name, eff)`: `with_span` + a `tracing` span (needed `install_tracing_layer`) | `OtelSpanHook`: a `SpanHook` installed with `eff.with_span_hook(..)`; every `eff.with_span(name)` becomes an OTEL span |
| starter | `install_otel_starter` installs globals + subscriber | also bridges `MetricRegistry::global()` (kept alive by the guard); `guard.span_hook()` returns the hook; `guard.metrics_bridge()` |
| capability | `provide_otel_runtime(guard) -> ProviderBox` | `provide_otel_runtime::<E>(guard) -> Layer<OtelRuntimeEnv, E>`; `need::<OtelRuntime>()` reads it |
| new | — | `current_otel_context()` (an effect) / `span_otel_context(&span)`: the OTEL `Context` of the current span, for W3C injection; `sdk_meter_provider_with_in_memory_exporter` is public |

```rust
// 0.6
let meter = guard.meter();
let requests = CounterBridge::new(Metric::counter("requests", tags), &meter, "requests");
let eff = with_span_otel("handle", requests.apply(1).flat_map(|_| work()));
run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
run_blocking(eff, ());

// 0.7
use id_effect::{Counter, Effect, Metric};
let guard = install_otel_starter(&providers, &config)?; // bridges MetricRegistry::global()
let requests = Counter::new("requests").tagged("route", "/users");
let eff: Effect<Out, Err> = requests
  .track_all(1, work())
  .with_span("handle")
  .with_span_hook(guard.span_hook());
TokioRuntime::current()?.run(eff).await;
```

- **Metrics mapping.** Counters become `u64` observable counters and gauges `i64` observable
  gauges, read from the registry at each collection (pull). Histograms become `f64` histograms with
  the registry's bucket boundaries, summaries `f64` histograms (default buckets), and frequencies
  `u64` counters with a `value` attribute; these are pushed on each record by a `MetricListener`.
  Tags, including fiber tags from `Effect::tag_metrics`, become attributes. The OTEL instrument
  name is the metric name.
- **Bridge lifetime.** The bridge lives as long as the returned `MetricsBridge`; dropping it
  detaches it. A series created before the bridge is attached is replayed (counter and gauge
  readings are complete; pushed kinds see the samples recorded after attaching).
- **Spans need the hook.** `with_span` alone records nothing in OTEL. Run the root effect under
  `with_span_hook(OtelSpanHook::for_provider(&provider, "svc").into_hook())` or
  `guard.span_hook()`. The hook is a `FiberRef`, so forked fibers on any executor thread export
  their spans under the right parent. For a request handler, start the trace from the incoming W3C
  headers with `OtelSpanHook::for_provider(..).with_parent(extract_trace_context_from_headers(..))`.
- **Span status.** Success is `Ok`, a typed failure or a defect is `Error`, and an interrupt leaves
  the status unset with `id_effect.interrupted = true`.
- **No `tracing` subscriber is needed for effect spans**, and no `install_tracing_layer`: the
  bridge calls the OTEL tracer directly. `tracing` spans and events (axum `trace_request`,
  `tracing::info!`) still go through the subscriber layers, but they are not parented under effect
  spans.
- **Logs.** `OtelLogBackend` is unchanged. When you install it through `id_effect_logger`'s
  thread-local `install_*` API, the logger caveat applies ([§14.4](#144-id_effect_logger)).

### 14.14 `id_effect_axum`

Handlers are `Effect`s. Because effects are `Send`, every request's effect runs as **one fiber on
the tokio runtime that serves the request** (a `TokioExecutor` over the ambient handle). The 0.6
`EffectExecutor` thread pool (one OS thread with a private `current_thread` runtime and a
`LocalSet` per in-flight request) is gone, and so is every `block_in_place` / `run_blocking` in
the crate.

| 0.6 | 0.7 |
|---|---|
| handler `FnOnce(&mut S) -> Effect<A, E, S>`, `S: Send` | the same with `S: Send + Sync`. `S` can be plain state, an HList of capabilities (`need::<Db>()`), or a `DynEnv` |
| `run_with_env` / `run_with_caps` / `run_with_request_caps` → `Result<A, E>`, a panic re-raised in the caller | → `Exit<A, E>` (`into_result()` for `Result<A, Cause<E>>`); a panic is `Cause::Die` |
| `execute` / `routing::*`: `Err(e)` → `e.into_response()`, a panic unwound the handler | a typed failure → `E::into_response()`; **a defect (any panic in the build closure or the effect) → `500`** with a `HandlerDefect { message }` response extension (the message is never sent to the client); an interrupt → `503`. `exit_response` / `cause_response` are public |
| client disconnect: the effect ran to completion (`DropPolicy::RunToCompletion`, the default) or was dropped at its next `.await` (`DropPolicy::Cancel`) | the request fiber is **interrupted** (`DropPolicy::Interrupt`, the default): its finalizers (`ensuring`, `acquire_release`, scoped layers) run. `DropPolicy::Detach` keeps the old run-to-completion behaviour |
| `EffectExecutorBuilder::{max_threads, queue_capacity, idle_timeout}`, `EffectExecutor::threads`, `run_local` / `try_run_local` | removed. `max_in_flight(n)` (unbounded by default) and `SaturationPolicy::{Wait, Reject}` (503) remain; new `runtime(rt)` pins a `Runtime` / `TokioRuntime`; `in_flight()`; `run` / `try_run` return `Exit`; `run_effect` / `try_run_effect` run an `Effect<A, E, HNil>` |
| request caps over `Env` + `CapabilityKey` (`CapOverrideLayer::<Cap<Tenant>, _>`) | over `DynEnv`, keyed by type: `CapOverrideLayer::new(\|parts\| Some(Tenant(..)))`, `RequestCaps::{insert, insert_arc, get}`; the state is `State<DynEnv>` |
| `routing::*_with_metrics(Metric<u64, ()>, Metric<Duration, ()>, f)` | `(Counter, Histogram, f)`; the histogram records seconds on the executor clock (failures are timed too) |
| `channel_bridge::{exchange, exchange_into_response(env, QueueChannel, req)}` | over `Channel<Response<Bytes>, Request<Bytes>>`; `exchange_into_response(ch, req)` (no env) |
| `service::EffectService` (`type Error = E`), `with_request_metrics(Metric, Metric)` | `type Error = Cause<E>` (a handler panic is `Cause::Die`); `with_request_metrics(Histogram, Counter)`; `S: Clone + Send + Sync`; the in-flight count and permit are released by a fiber finalizer |
| `service::ChannelService<S, Req, Res>::new(state, QueueChannel)` | `ChannelService<Req, Res>::new(Channel<Res, Req>)` |
| `server::{load_host_config::<R: Needs<..>>(), provide_host_config_env() -> ProviderBox, bootstrap_env(p) -> Env}`; `Host::env: Env` | `load_host_config::<R, I>()` → `Effect` (`R: Selector<ConfigProviderService, I>`); `provide_host_config_env::<E>()` → `Layer<ConfigEnv, E>`; `bootstrap_env(p)` → `ConfigEnv`; `HostConfig::{config(), layer::<E>()}`; `Host::env: ConfigEnv`; `HostError::Defect` |
| — | the server as a scoped layer: `server_layer(app)` (binds on acquire, graceful shutdown within `shutdown_timeout` on release, provides `ServerHandle { local_addr() }`), `serve(app)` (= `launch`), `serve_until(app, stop)`, `serve_until_shutdown(app)` (SIGINT / SIGTERM) |
| — | `body::{stream_body, effect_body, body_stream}`: response bodies from a `Stream<Bytes, E>` (dropping the body interrupts the producer fiber), request bodies as a stream |

Unchanged: `json`, `health`, `server::{security, shutdown}`, the `cluster` routes, `serve_router`,
`HostBuilder::run_until_shutdown` and the `tower` feature (`service` module). `id_effect_tokio`
is no longer a dependency (a `TokioRuntime` converts into the `Runtime` that
`EffectExecutorBuilder::runtime` takes).

```rust
// 0.6
use id_effect::{Cap, Effect, Env, succeed};
fn whoami(env: &mut Env) -> Effect<String, Infallible, Env> {
  succeed(env.get::<Cap<Tenant>>().0.clone())
}
let app = Router::new()
  .route("/whoami", get(|st: State<Env>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)))
  .layer(CapOverrideLayer::<Cap<Tenant>, _>::new(|p| Some(Tenant(tenant_of(p)))))
  .with_state(base_env);
let n: Result<u32, E> = run_with_env(state, |s| count(s)).await;

// 0.7
use id_effect::{DynEnv, Effect, HCons, HNil, hlist, need, succeed};

// Typed capabilities as router state:
type AppEnv = HCons<Db, HNil>;
fn users(_: &mut AppEnv) -> Effect<Json<Vec<User>>, ApiError, AppEnv> {
  need::<Db>().flat_map(|db| db.list_users().narrow()).map(Json)
}
let app = Router::new().route("/users", routing::get(users)).with_state(hlist![db]);

// Runtime-keyed request caps:
fn whoami(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
  succeed(env.try_get::<Tenant>().map_or("none".into(), |t| t.0.clone()))
}
let app = Router::new()
  .route("/whoami", get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)))
  .layer(CapOverrideLayer::new(|p| Some(Tenant(tenant_of(p)))))
  .with_state(DynEnv::new().with(Tenant("public".into())));
let n: Result<u32, Cause<E>> = run_with_env(state, |s| count(s)).await.into_result();

// The server's lifetime is a scoped layer:
let config = provide_host_config_env::<HostError>().provide_to(HostConfig::layer());
let program = serve_until_shutdown(app).provide_layer(config);
let exit = EffectExecutor::global().run_effect(program).await;
```

## 15. Labs crates

The labs crates (`labs/`, unpublished, git dependencies only) are migrated to the 0.7 kernel with
no new features: their public signatures differ only in their kernel types, plus the `Send` /
`Sync` bounds and the `Selector` index parameter the kernel needs. Take them from the same
revision as the published crates.

**A blocking edge inside async code panics.** Several labs APIs are synchronous and run their
effect on `Runtime::global()` with `block_on`: `id_effect_events`' `dispatch_command`,
`query_projection` and `run_projection_from_store`, and `id_effect_workflow`'s
`DuroxideStepJournal`. Unlike 0.6 `run_blocking`, they must not be called from inside an async
context (a tokio task, `spawn_blocking`, an entered runtime): there the run is refused and the
call panics with the `BlockOnInAsync` defect. Use the async APIs from async code, or move the
work to a plain thread.

### `id_effect_events`

`EventStore::{append, read, latest_version}`, `CommandHandler::handle` and `QueryHandler::query`
return `Effect<A, EventStoreError>` / `Effect<_, Self::Error>` (`R = HNil`), and
`MemoryEventStore`, `FileJournal`, `SqlEventJournal` and `EsEntityEventStore` implement them.
`ProjectionRunner::run_all` and `dispatch_command_es_entity` (async) keep their signatures.

```rust
// 0.6
use id_effect::{Effect, fail, run_blocking, succeed};
impl CommandHandler<Deposit, Evt> for DepositHandler {
  type Error = Reject;
  fn handle(&self, cmd: Deposit) -> Effect<Vec<Evt>, Reject, ()> {
    if cmd.amount <= 0 { return fail(Reject); }
    succeed(vec![Evt::Deposited(cmd.amount)])
  }
}
let stored = run_blocking(store.append("acct", &[Evt::Deposited(1)]), ())?;

// 0.7
use id_effect::{Effect, Runtime, fail, succeed};
impl CommandHandler<Deposit, Evt> for DepositHandler {
  type Error = Reject;
  fn handle(&self, cmd: Deposit) -> Effect<Vec<Evt>, Reject> {
    if cmd.amount <= 0 { return fail(Reject); }
    succeed(vec![Evt::Deposited(cmd.amount)])
  }
}
let stored = store.append("acct", &[Evt::Deposited(1)]).block_on().into_result()?; // sync
let stored = Runtime::global().run(store.read("acct", 1)).await.into_result()?;      // async
```

With the `es-entity` feature, `provide_es_entity_events(backend)` and
`provide_es_entity_events_from_pool(pool)` keep their names and arguments but return a
`Layer<EventJournalEnv, E>` (`EventJournalEnv = HCons<EventJournalBackend, HNil>`) instead of a
`ProviderBox`. `Needs::<EventJournalBackend>::need(&env)` is `need::<EventJournalBackend>()`:

```rust
// 0.6
let env = build_env([provide_es_entity_events_from_pool(pool)])?;
let backend = Needs::<EventJournalBackend>::need(&env).clone();

// 0.7
use id_effect::{Effect, need};
let program: Effect<_, MyError, EventJournalEnv> =
  need::<EventJournalBackend>().flat_map(|slot| use_backend(EventJournalBackend::clone(&slot)).narrow());
let exit = program.provide_layer(provide_es_entity_events_from_pool(pool)).block_on();
```

### `id_effect_parse` and `id_effect_graph`

The schema bridge is unchanged (`id_effect::schema` is kernel-neutral). `parse_stream` /
`parse_text_stream` take a `Stream<Chunk<_>, E, R>` and return an `Effect<O, ParseStreamError<E,
P>, R>`, with `R: Send + Sync + 'static`. A stream that cannot fail needs only the item type
(`Stream::<Chunk<u8>>`), because `E` defaults to `Never`:

```rust
// 0.6
use id_effect::{Chunk, Stream, run_blocking};
let stream = Stream::from_iterable(vec![Chunk::from_vec(b"hi".to_vec())]);
let value = run_blocking(parse_text_stream(tag("hi"), stream), ())?;

// 0.7
use id_effect::{Chunk, Stream};
let stream = Stream::<Chunk<u8>>::from_iterable(vec![Chunk::from_vec(b"hi".to_vec())]);
let value = parse_text_stream(tag("hi"), stream).block_on().into_result(); // Result<_, Cause<_>>
let err = value.unwrap_err().fail_option(); // Some(ParseStreamError::..) on a typed failure
```

`id_effect_graph` has no `id_effect` dependency and is unchanged.

### `id_effect_jobs`

`JobRunner::{enqueue, dequeue, complete, fail, pending_count}`,
`FabricJobRunner::{enqueue_fabric, dequeue_for, pending_for}`,
`OutboxTable::{insert, fetch_unpublished, mark_published, unpublished_count}`,
`MessageBroker::publish`, `JobQueue::enqueue` / `ApalisJobQueue::enqueue` and
`ObixOutbox::insert` / `ObixInbox::persist` return `Effect<A, JobError>` (or their own error)
with `R = HNil`. `drain_jobs(runner, limit, handler)` and `relay_outbox(outbox, limit, publish)`
take `Send` closures and build one effect instead of a nested `run_blocking` per job. A typed
handler failure still marks the job failed and moves on; a handler defect or interrupt now
propagates out of the drain (0.6 `run_blocking` panicked). The sqlx-backed effects (apalis, obix)
need a tokio runtime with its IO driver.

```rust
// 0.6
let rec = run_blocking(runner.enqueue(JobSpec::new("a", b"1")), ())?;
let n = run_blocking(drain_jobs(runner.clone(), 10, |_| succeed::<(), JobError, ()>(())), ())?;
let row = run_async(obix.insert(record), ()).await?;

// 0.7
use id_effect::succeed;
use id_effect_tokio::TokioRuntime;
let rec = runner.enqueue(JobSpec::new("a", b"1")).block_on().into_result()?;
let n = drain_jobs(runner.clone(), 10, |_| succeed::<(), JobError, _>(())).block_on().into_result()?;
let row = TokioRuntime::current()?.run(obix.insert(record)).await.into_result()?;
```

**The `apalis` feature** did not compile at 0.6: it was written for `apalis-postgres` 1.0.0-rc.8,
while the lockfile resolves 1.0.0 (on sqlx 0.9). It now uses the 1.0.0 API
(`PostgresStorage::<()>::setup`, `PostgresStorage::new(pool).with_config(..)`), and
`ApalisJobQueue::{setup, new, pool}` take and return an `apalis_postgres::PgPool` (sqlx 0.9)
instead of a `sqlx::PgPool` (0.8). The obix adapters stay on sqlx 0.8, so a program that uses
both opens two pools:

```rust
let pool = pg_pool_from_config(PgPoolConfig::from_url(url.clone())).await?; // sqlx 0.8: obix
let apalis_pool = apalis_postgres::PgPool::connect(&url).await?;          // sqlx 0.9: apalis
ApalisJobQueue::setup(&apalis_pool).await?;
let queue = ApalisJobQueue::new(&apalis_pool, "demo");
```

Build the pg features with `SQLX_OFFLINE=true`: `apalis-postgres` checks its `query_file!` macros
against `DATABASE_URL` at compile time, which fails on a database without the `apalis` schema.

### `id_effect_workflow`

`provide_duroxide_pg(pool, url)` returns a
`Layer<HCons<DuroxideProvider, HCons<WorkflowRuntime, HNil>>, E>` (`Layer::succeed`; the caller
owns the pool) instead of a `ProviderBox`. `StepJournal`, `DurableWorkflowLog`,
`NetworkJournalStub`, `RemoteStepJournal`, `JournalServer`, the `fsm` helpers (`register_fsm`,
`step_durable`, `restore_state`) and `bootstrap_duroxide_schema` are unchanged.

**`DuroxideStepJournal` is a blocking edge.** In 0.6 it used tokio `block_in_place` +
`Handle::block_on`, so it had to be called from inside a multi-thread tokio runtime. In 0.7 each
statement runs on a fiber of `Runtime::global()` while the calling thread parks, so it must be
called from a plain thread. A `run_step_typed` transaction still spans the lookup, `compute` and
the insert; a failed or panicking `compute` rolls it back.

```rust
// 0.6 (inside #[tokio::main] / #[tokio::test(flavor = "multi_thread")])
let mut journal = DuroxideStepJournal::new(pool);
journal.register_workflow(&wf)?;
let step: String = journal.run_step_typed(&wf, 0, "s", || Ok("done".into()))?;

// 0.7: off the async runtime
let step = std::thread::spawn(move || {
  let mut journal = DuroxideStepJournal::new(pool);
  journal.register_workflow(&wf)?;
  journal.run_step_typed::<String, _>(&wf, 0, "s", || Ok("done".into()))
})
.join()
.expect("journal thread")?;

// providers: 0.6 build_env([provide_duroxide_pg(pool, url)])?
let program = program.provide_layer(provide_duroxide_pg::<AppError>(pool, url));
```

### `id_effect_rpc`

| item | 0.6 | 0.7 |
|---|---|---|
| `RpcGroup::register(.., handler)` | `handler: Fn(A, &mut Env) -> Effect<B, RpcError, Env>` | `handler: Fn(A, &mut DynEnv) -> Effect<B, RpcError, DynEnv>` |
| `RpcMethodEntry::dispatch(payload, env)` | `env: Env` | `env: DynEnv` |
| `RpcServer::{mount_dispatch, mount_default}`, `layer_rpc` | `Router<Env>` | `Router<DynEnv>` |
| `RpcClient::call::<.., R>(..)` | `R: Needs<HttpClientService>` | `call::<.., R, I>(..)`, `R: Selector<HttpClientService, I> + HList` |
| `codegen::emit_service_trait` output | `-> Effect<T, RpcError, ()>` | `-> Effect<T, RpcError>` |

A handler's effect runs as one request fiber on the tokio runtime that serves the request
(`id_effect_axum::run_with_env`). A typed `RpcError` still becomes a failure envelope. **A defect
(a panic in the handler or its effect) or an interrupt now becomes an `RpcError::internal` failure
envelope** (HTTP 500, without the panic message); 0.6 re-raised the panic in the axum handler.

```rust
// 0.6
group.register("greet", payload, &["name", "n"], success, &["message", "n"],
  |(name, n), _env: &mut Env| succeed((format!("Hello, {name}!"), n)));
let app = RpcServer::new(group).mount_default().with_state(Env::new());
let out = run_async(client.call("greet", input, &p, &pf, &s, &sf), env).await?;

// 0.7
use id_effect::{DynEnv, succeed};
use id_effect_platform::http::{ReqwestHttpClient, http_client_env};
group.register("greet", payload, &["name", "n"], success, &["message", "n"],
  |(name, n), _env: &mut DynEnv| succeed((format!("Hello, {name}!"), n)));
let app = RpcServer::new(group).mount_default().with_state(DynEnv::new());
let out = client
  .call("greet", input, &p, &pf, &s, &sf)
  .provide_all(http_client_env(ReqwestHttpClient::default_client()))
  .block_on() // or TokioRuntime::current()?.run(..).await in async code
  .into_result()?;
```

### `id_effect_ai`

| item | 0.6 | 0.7 |
|---|---|---|
| `LanguageModel::{complete, complete_stream}` | `Effect<_, AiError, ()>`; the stream is a `Stream<CompletionChunk, AiError, ()>` | `Effect<_, AiError>`; `Stream<CompletionChunk, AiError>` |
| `complete::<R>`, `complete_stream::<R>` | `R: Needs<LanguageModelService>` | `complete::<R, I>`, `R: Selector<LanguageModelService, I> + HList` |
| `mock_chunk_stream` | a stream fed by a thread (`run_blocking` per chunk) | `Stream::from_vec` (no thread) |
| `provide_mock_language_model()` | `ProviderBox` | `provide_mock_language_model::<E>() -> Layer<HCons<LanguageModelService, HNil>, E>` |
| `provide_openai_language_model(client, cfg)`, `provide_anthropic_language_model(..)` | `Result<ProviderBox, AiError>` | `::<E>(..) -> Result<Layer<HCons<LanguageModelService, HNil>, E>, AiError>` |
| `provide_cursor_agents_client(client, cfg)` | `Result<ProviderBox, AiError>` | `::<E>(..) -> Result<Layer<HCons<CursorAgentsClientService, HNil>, E>, AiError>` |
| `CursorAgentsClient` methods, `cursor::models::list_models` | `Effect<_, AiError, ()>` | `Effect<_, AiError>` |
| `default_ai_retry_schedule()` | the 0.6 `Schedule` | `Schedule<(Duration, u64), AiError>` (`exponential(50ms).intersect(recurs(3))`) |
| `retry_transient_ai_http(make)` | `make: Fn() -> Effect + Send + Clone` | `make: Fn() -> Effect + Send + Sync` (`Effect::retry` with `while_input(AiError::is_transient)`) |
| `with_ai_request_span`, `with_ai_span` | the free `with_span` | `Effect::with_span("ai.request")` (exported by the fiber's `SpanHook`); `R: Send + Sync` |

`LanguageModelService` / `CursorAgentsClientService` stay `Arc<dyn …>`. The HTTP adapters call
`id_effect_platform`'s effects directly, so they run on any runtime with a tokio IO driver.
`retry_transient_ai_http` now waits the schedule's delays (50, 100, 200 ms) on the fiber's clock;
0.6 parked the thread for `25 ms × attempt` and ignored the schedule's delays. `wait_until_idle`
polls with `sleep` (200 ms) instead of `tokio::time::sleep`.

```rust
// 0.6
let env = build_env([provide_openai_language_model(client, cfg)?])?;
let resp = run_async(complete(req), env).await?;

// 0.7
use id_effect::{HCons, HNil};
let layer = provide_openai_language_model::<AiError>(client, cfg)?;
let resp = complete::<HCons<LanguageModelService, HNil>, _>(req)
  .provide_layer(layer)
  .block_on() // or TokioRuntime::current()?.run(..).await
  .into_result()?; // Result<_, Cause<AiError>>
```

### `id_effect_lint`

The Dylint rules recommend the 0.7 API: D-01 flags `block_on` / `try_block_on` outside `main`,
D-02 (`run_test`) is an allow-by-default hint that suggests `TestExecutor`, I-02 flags
`.into_result().unwrap()`, and the `#[capability]` advice names `need::<Key>()` / `Layer`. The
lint names are unchanged.

## 16. Removed items and their replacements

Items with a 0.7 namesake (`Effect`, `Stream`, `Sink`, `Queue`, `Channel`, `Scope`, `FiberRef`,
`FiberHandle`, `Pool`, `Cache`, `Schedule`, `Runtime`, `Supervisor`, `Metric`, …) resolve to it,
with the 0.7 API described above. These are removed without a namesake:

| Removed in 0.7 | Replacement |
|---|---|
| crates `id_effect_tower`, `id_effect_sql_pg` | `id_effect_axum::service` (feature `tower`), `id_effect_sql::pg` (feature `pg`) |
| `StreamV1`, `ComputeSupervisor`, `fabric.supervisor()`, `id_effect_platform::process` | `Stream`, `FabricGovernor`, `fabric.governor()`, `id_effect_platform::command` |
| `#[derive(SchemaParser)]`, `#[capability(..)]` | `#[derive(Schema)]`; delete the attribute (the service type is the key) |
| root `pipe1` / `pipe2` / `pipe3`, `flip`, `const_`, `tupled`, `untupled`, `memoize`, `MutableRef` | `id_effect_fp::foundation::function::*`; `std::sync::Mutex` / `FiberRef` / `SynchronizedRef` |
| module aliases `id_effect::{channel, ref_, either, func, clock, duration, schedule, data, stream, snapshot}` | the real module path or the root item ([§3.7](#37-module-paths)) |
| `run_blocking`, `run_async`, `run_fork`, `run_test`, `run_test_with_clock` | `Effect::block_on` at the edge, `Runtime::run(eff).await`, `Effect::fork` / `Runtime::fork`, `TestExecutor` |
| `ThreadSleepRuntime`, the 0.6 `Runtime` trait, `yield_now` | `Runtime`, `TokioExecutor`, `TestExecutor`, `yield_fiber`, `sleep` |
| `Env`, `Cap`, `CapList`, `Caps`, `Needs`, `HasCap`, `FromEnv`, `Capability*`, `CapBind*`, `CapKeys`, `CapWiden`, `NoCaps`, `cap_into_bind` | typed `R`: `HCons` / `HNil`, `caps!`, `need::<K>()`, `~K` in `effect!`, `Selector`, `provide` / `provide_env` / `provide_all` / `narrow`; `DynEnv` for runtime-keyed capabilities |
| `ProviderSpec`, `ProviderBox`, `ProviderNode`, `Provider`, `ProviderError`, `CapabilityGraph`, `PlannerNode`, `PlannerPlan`, `plan_topological`, `build_env`, `run`, `run_with`, `RunError`, `with_override`, `with_fiber_and_override`, `on_shutdown` | `Layer` (`merge`, `provide_to`, `provide_merge`, `provide_layer`, `acquire_release`); a missing capability is a compile error |
| `id_effect_macro::{caps, provide, providers, require, mock_capability}` | the root `caps!`, `P::layer()` / `Layer::succeed`, `Layer::merge`, a plain value via `provide`, `~Key` / `need::<Key>()` |
| `#[named(..)]` provider variants, `with = \|deps: &Env\| ..` constructors | distinct newtypes or `DynEnv`; `with = \|\| ..` |
| `IntoBind`, `into_bind`, `pure`, `box_future`, `flatten_or`, `join_binds2/3/4`, `#[serial]`, `scope_with`, the free `scoped` / `acquire_release` | `succeed`, `from_async`, `~(a, b)` in `effect!`, `zip_par`, `Effect::scoped`, `acquire_use_release` |
| `Effect::new(\|r: &mut R\| ..)` | `need::<K>()`, `sync`, `from_async` |
| `CancellationToken`, `check_interrupt`, `FiberStatus`, `fiber_all`, `fiber_succeed`, `fiber_never`, `interrupt_all`, `with_fiber_id` | fiber interruption (`FiberHandle::interrupt`, `interruptible` / `uninterruptible`), `all` / `fork_all` |
| `SupervisorPolicy` with a `Schedule` and a `CancellationToken` | `SupervisorPolicy::{Restart, RestartWithLimit}` with a fixed `delay`, the 0.7 `Supervisor` |
| `QueueChannel`, `ChannelReadError`, `Channel::{from_stream, from_sink, from_fold}` | `Channel` (duplex), `StreamChannel`, `FoldChannel`, `channel.to_stream()` |
| `ref_::make`, `latch::open`, `semaphore::acquire`, `Ref::make`, `Semaphore::make`, `Deferred::make`, `acquire_owned`, … | plain constructors (`Ref::new`, `Semaphore::new`, `Deferred::new`, …), `Semaphore::acquire` returns a `Permit` |
| `Pool::make`, `make_with_ttl`, `Cache::make`, `CacheStats::{evictions, loads}` | `Pool::new(..).with_ttl(..)`, `Cache::new`, `stats()` = `{ hits, misses, size }` |
| `request_resolver`: `RequestEntry`, `FnRequestResolver`, `make_request_resolver`, `batching`, `MissingKey`, `BatchError` | the `request` module: `Request`, `RequestResolver::make_batched`, `Effect::request`, `RequestCache` |
| `Finalizer` | `Scope::add_finalizer` / `add_finalizer_exit` |
| `Clock`, `LiveClock`, `TestClock`, `SpinClock` | the executor clock (`TestExecutor::advance`) |
| `repeat`, `repeat_n`, `retry`, `forever`, `*_with_clock`, `*_with_clock_and_interrupt`, `ScheduleDecision`, `ScheduleInput`, `Schedule::compose` | `Effect::repeat` / `Effect::retry` (+ `_or_else`) with a typed `Schedule`; `intersect` |
| the 0.6 `Metric<In, Out>`, `metric_make`, `with_span` (free fn), `AnnotateCurrentSpanSuccess`, `AnnotateCurrentSpanErr`, `TracingFiberRefs`, `record_span_annotations` | `Counter` / `Gauge` / `Histogram` / `Summary` / `Frequency`, `MetricRegistry`, `Effect::with_span` + a `SpanHook` (`tracing_hook()`), `annotate_current_span` |
| `Stream::new(\|env\| fut)`, `poll_next_chunk`, `with_throughput_metric` | `from_effect` / `from_pull` / `unfold_effect`, `next_chunk`, `with_throughput_counter` |
| `testing::{snapshot, proptest, stream}` (`SnapshotAssertion`, `assert_golden*`, `assert_exit_*`, `arb_exit_*`, `assert_bounded_memory`, leak and unclosed-scope recorders), the `proptest` feature | `TestExecutor`, assertions on `Exit` values; a pull stream with `take(n)` on an infinite source shows bounded memory |
| `id_effect_tokio`: `spawn_blocking_run_async`, `TokioRuntime::{with_fabric, fabric, sleep}`, `yield_now` | `rt.spawn(eff)`, the ambient `Fabric`, `id_effect::sleep`, `id_effect::yield_fiber` |
| `id_effect_cli`: the `id-effect-diagnose` binary | none: a missing capability is a compile error |
| `id_effect_config`: the free loaders and combinators, `read_*`, `WithConfigDefault`, the `config` module | `Config<T>` ([§14.3](#143-id_effect_config)) |
| `id_effect_axum`: `EffectExecutorBuilder::{max_threads, queue_capacity, idle_timeout}`, `EffectExecutor::threads`, `run_local`, `try_run_local`, `DropPolicy::{RunToCompletion, Cancel}` | `max_in_flight`, `runtime(rt)`, `DropPolicy::{Interrupt, Detach}` |
| `id_effect_platform`: `env_set_http_client`, `env_has_http_client`, `response_body_chunk`, `body_as_chunk_stream` | `http_client_env` / `provide_http_client`, `resp.body`, `HttpBody::from_bytes` |
| `id_effect_process`: `ProcessNode::{threads, with_spawner, clock}`, `Job`, `Spawner`, `ProcessCell::cancellation` | `ProcessNode::{new, on_runtime, on_executor, now}` |
| `id_effect_resilience`: `make`, `make_with_clock`, `retry_with_breaker_and_clock` | `new`, the executor clock, `retry_with_breaker` |
| `id_effect_opentelemetry`: `CounterBridge`, `DurationHistogramBridge`, `with_span_otel` | `bridge_metrics` / `bridge_global_metrics`, `OtelSpanHook` |
| `id_effect_sql`: `examples/010_axum_sql.rs` | `examples/http-showcase` |
