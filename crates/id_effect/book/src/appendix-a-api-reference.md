# API Quick Reference

A condensed reference for the most commonly used types and functions in id_effect 0.7. Every
module's items are re-exported at the crate root (`use id_effect::{Effect, Layer, Stream, …}`).
For full documentation, run `cargo doc --open -p id_effect`.

## Core Types

| Type | Description |
|------|-------------|
| `Effect<A, E = Never, R = HNil>` | A lazy, `Send`, one-shot computation that produces `A`, can fail with `E`, and needs the capabilities in `R` |
| `LocalEffect<A, E, R>` | The same for `!Send` bodies (`Rc`, `RefCell` across awaits); built with `effect_local!` |
| `Stream<A, E, R>` | A pull-based, chunked sequence of `A` values |
| `Exit<A, E>` | The outcome of running an effect: `Success(A)` or `Failure(Cause<E>)` |
| `Cause<E>` | `Fail(E)`, `Die(Defect)`, `Interrupt(FiberId)`, composed with `Both` / `Then` |
| `Never` | The error type of an effect that cannot fail |
| `HCons<H, T>` / `HNil` | The typed environment: an HList of capabilities |
| `caps!(A, B)` | Spells `HCons<A, HCons<B, HNil>>` for `Effect<_, _, caps!(A, B)>` |
| `Layer<Out, E, In>` | A recipe that builds the capabilities `Out` from `In` (memoised per build, scoped finalizers) |
| `Chunk<A>` | A contiguous batch of `A` values |
| `Unknown` | Unvalidated wire data; the input type for schemas |
| `ParseErrors` | Accumulated parse failures with paths |

## Capabilities and Layers

| Item | Notes |
|------|-------|
| `~Cap` inside `effect!` | Read capability `Cap` from `R` |
| `need::<Cap>()` | The same outside `effect!` (`.map(..)`, `.flat_map(..)`, `.effect()`) |
| `eff.provide(v)` | Remove `v`'s type from `R` by supplying it |
| `eff.provide_env(hlist![a, b])` | Supply the whole environment; `R` becomes `HNil` |
| `eff.provide_some(hlist![..])` / `eff.narrow()` | Supply part of `R` / run a narrower effect in a wider `R` |
| `eff.provide_layer(layer)` | Build `layer` and supply its output |
| `Layer::succeed(hlist![..])` | A layer from ready values |
| `Layer::effect(\|\| eff)` / `Layer::scoped(\|scope\| eff)` / `Layer::acquire_release(acq, rel)` | Effectful, scoped and bracketed layers |
| `a.merge(b)` / `a.provide_to(b)` / `a.provide_merge(b)` | Compose layers side by side / feed one into another |
| `#[derive(ProviderSpecDerive)]` + `#[provides(Cap, with = \|\| ..)]` | Generates an inherent `layer()` |
| `DynEnv` / `try_need::<T>()` | A runtime-checked environment for dynamic edges (Axum state) |

A missing capability is a compile error, not a runtime one.

## Constructors

| Function | Notes |
|----------|-------|
| `succeed(a)` | Always succeeds with `a` |
| `fail(e)` | Fails with the typed error `e` |
| `die(defect)` | Fails with a defect (`Cause::Die`) |
| `sync(\|\| a)` | Runs a synchronous closure when the effect runs |
| `suspend(\|\| eff)` | Builds the effect lazily |
| `from_async(\|\| async { .. })` | Lifts a future returning `Result<A, E>` |
| `sleep(d)` / `yield_fiber()` | Sleep on the executor clock / yield the fiber |
| `effect!({ .. })` | Do-notation: `~expr` binds, `~Cap` reads, `~(a, b)` joins in parallel, `?` on a `Result` |
| `Stream::from_iterable(i)` / `Stream::from_vec(v)` / `Stream::range(a, b)` | Finite streams |
| `Stream::unfold(s, f)` / `Stream::unfold_effect(s, f)` | Generate a stream from state, lazily |
| `Stream::from_pull(f)` / `Stream::from_effect(eff)` / `Stream::from_queue(q)` | Custom sources |

## Effect Combinators

| Method | Notes |
|--------|-------|
| `.map(f)` / `.flat_map(f)` / `.then(next)` | Transform / chain / sequence |
| `.map_err(f)` | Transform the error |
| `.catch(f)` | Recover from a typed failure |
| `.exit()` | Observe the outcome as an `Exit` value |
| `.zip(other)` | Run sequentially, tuple the results |
| `.widen()` | Adapt the error type (e.g. from `Never`) |
| `.ensuring(fin)` | Run a finalizer on every exit |
| `.interruptible()` / `.uninterruptible()` | Interrupt masks |
| `.timeout(d)` / `.timeout_fail(d, \|\| e)` | `Option<A>` on timeout / fail on timeout |
| `.race(other)` | First success wins; the loser is interrupted |
| `.with_span(name)` | Record a span (seen by the installed `SpanHook`) |
| `Effect::retry(\|\| eff, schedule)` | Re-run a fresh effect from the factory on failure |
| `Effect::repeat(\|\| eff, schedule)` | Re-run on success (`retry_or_else` / `repeat_or_else` handle exhaustion) |

Effects are one-shot values: anything that runs an effect more than once (`retry`, `repeat`,
`Layer::effect`, `supervised`) takes a factory closure.

## Concurrency

| Function/Method | Notes |
|----------------|-------|
| `eff.fork()` / `eff.fork_daemon()` | Start a child (or detached) fiber; returns a `FiberHandle` |
| `handle.join()` | Wait for the fiber's outcome |
| `handle.interrupt()` | Interrupt the fiber; returns its `Exit` |
| `zip_par(a, b)` / `zip_with_par(a, b, f)` | Run two effects in parallel |
| `all(effects, Concurrency::unlimited())` / `for_each_par(..)` / `fork_all(..)` | Run many effects in parallel |
| `race_all(effects)` / `race_first(effects)` | First success / first exit wins |
| `FiberRef::new(init)` / `.get()` / `.set(v)` / `.locally(v, eff)` | Fiber-scoped dynamic variables |
| `Semaphore::new(n)` / `sem.with_permit(eff)` | Bound concurrency |
| `Queue`, `Channel`, `Deferred`, `Latch`, `PubSub`, `Ref`, `SynchronizedRef`, `SubscriptionRef` | Coordination primitives |
| `supervised(policy, make)` | Run a fresh `make()` under a `SupervisorPolicy` (`Terminate`, `Restart { delay }`, `RestartWithLimit { .. }`, …) |
| `Supervisor::new()` / `sup.spawn(policy, make)` / `sup.shutdown()` | Own supervised fibers; shutdown interrupts them all |

## STM (`id_effect_fp`)

STM lives in the maintenance-tier `id_effect_fp` crate.

| Function | Notes |
|----------|-------|
| `TRef::make(v)` | Create a transactional cell (an `Stm`) |
| `tref.read_stm()` / `tref.write_stm(v)` | Read / write inside a transaction |
| `tref.modify_stm(f)` / `tref.update_stm(f)` | Modify inside a transaction |
| `commit(stm)` / `atomically(stm)` | Lift an `Stm<A, E>` into an `Effect<A, E, R>` |
| `Stm::retry()` / `Stm::check(cond)` | Restart the transaction until a condition holds |
| `Stm::fail(e)` | Abort with an error; nothing is written |
| `a.or_else(b)` | Run `b` when `a` retries |
| `TQueue::bounded(n)` / `TQueue::unbounded()` / `TMap::make()` / `TSemaphore::make(n)` | Transactional collections |

## Resources

| Function | Notes |
|----------|-------|
| `acquire_use_release(acq, use_, rel)` | Bracket-style resource management |
| `Effect::scoped(\|scope\| eff)` | Run with a `Scope`; its finalizers run when it closes |
| `scope.add_finalizer(fin)` / `scope.add_finalizer_exit(f)` | Register finalizers |
| `Pool::new(capacity, \|\| acquire)` / `pool.get(&scope)` | Reusable resource pool (`KeyedPool` per key) |
| `Cache::new(capacity, ttl, \|k\| lookup)` / `cache.get(k)` | Cache filled by an effect |

## Scheduling

| Function | Notes |
|----------|-------|
| `Schedule::spaced(d)` | Wait `d` between attempts |
| `Schedule::exponential(base)` / `exponential_by(base, f)` / `fibonacci(one)` | Backoff |
| `Schedule::recurs(n)` / `Schedule::forever()` | Limit the number of recurrences |
| `.intersect(other)` / `.union(other)` / `.and_then(other)` | Combine schedules |
| `.while_input(p)` / `.up_to(max)` / `.jittered()` | Stop on a predicate / cap the elapsed time / randomise delays |

## Running Effects

| Function | Notes |
|----------|-------|
| `eff.block_on()` | Run an `Effect<A, E>` (`R = HNil`) at the program edge; returns `Exit<A, E>` |
| `Runtime::global().run(eff).await` | The async edge, inside async code |
| `Runtime::new()` / `Runtime::from_executor(exec)` | A runtime on its own Tokio runtime / on any `Executor` |
| `TestExecutor::new()` | Deterministic executor with a virtual clock: `run_until_stalled`, `advance(d)` |
| `eff.fork_root(Arc::new(exec.clone()))` | Start a root fiber on an executor (tests use it with `TestExecutor`) |

## Schema

| Function | Notes |
|----------|-------|
| `string()` / `i64()` / `f64()` / `bool_()` | Primitive schemas |
| `optional(s)` / `array(s)` / `null_or(s)` / `record(s)` | Containers |
| `struct_(…)` / `tuple(…)` / `union_(…)` | Composite schemas |
| `refine(s, pred, msg)` / `filter(..)` / `transform(..)` | Constraints and conversions |
| `schema.decode_unknown(&unknown)` | Run a schema on wire data |
| `#[derive(Schema)]` | Generate `schema()` + `HasSchema` for a struct |
| `schema::serde_bridge` | `serde_json::Value` → `Unknown` (feature `schema-serde`) |

## Macros

| Macro | Notes |
|-------|-------|
| `effect!({ .. })` | Do-notation for `Effect` |
| `effect_local!({ .. })` | Do-notation for `LocalEffect` |
| `caps!(A, B)` | The typed environment type |
| `hlist![a, b]` | An environment value |
| `pipe!(v, f, g)` | Left-to-right application for pure values |
| `err!(A \| B)` | Type-level error union `Or<A, B>` |

## Workspace crates (beyond `id_effect`)

Crates are grouped by tier ([ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md)). Everything except labs is published to crates.io in lockstep.

| Crate | Tier | Role | Book |
|-------|------|------|------|
| `id_effect` | core | `Effect`, typed `R`, `Layer`, fibers, streams, schema, `TestExecutor` | Parts I–IV |
| `id_effect_macro` / `id_effect_proc_macro` | core | `pipe!` / `err!`; `effect!`, `caps!` and the derives | [Workspace tooling](./appendix-d-workspace-tooling.md) |
| `id_effect_tokio` | core | `TokioRuntime` (owned or ambient Tokio runtime; `run`, `fork`, `spawn`) | [Tokio bridge](./part2/ch07-05-tokio-bridge.md) |
| `id_effect_config` | core | Config descriptors, Figment, a config provider in `R` | [Configuration](./part2/ch07-10-config.md) |
| `id_effect_logger` | core | Injectable `EffectLogger` | [Logging](./part2/ch07-11-logger.md) |
| `id_effect_resilience` | core | Circuit breaker, rate limiter, bulkhead, `retry_with_breaker`, hedging | [Runtime resilience](./part5/ch21-00-runtime-resilience.md) |
| `id_effect_process` | runtime | OTP-style processes, links, monitors, supervision trees | [Processes and supervision](./part7/ch37-00-processes-supervision.md) |
| `id_effect_node` | runtime | Distribution: membership, remote processes, placement | [Distribution](./part7/ch38-00-distribution.md) |
| `id_effect_fsm` | runtime | State machines, `#[derive(Fsm)]`, `FsmProcess`, sagas | [State machines](./part5/ch19-00-state-machines.md) |
| `id_effect_platform` | integration | HTTP / FS / subprocess (`command`) capabilities; `auth` feature | [Platform I/O](./part2/ch07-06-platform-services.md) |
| `id_effect_axum` | integration | Axum host, `EffectExecutor`; `tower` feature for `service` | [Axum host](./part2/ch07-08-axum-host.md) |
| `id_effect_sql` | integration | `SqlClient`, transactions; `pg` feature for Postgres | [Data access](./part6/ch28-00-data.md) |
| `id_effect_jobs` | integration | Postgres job queue, workers, retries, unique keys; `cron` feature | [Background jobs](./part6/ch31-01-jobs.md) |
| `id_effect_messaging` | integration | `MessageBroker`, `MemoryBroker`, transactional outbox, relay, inbox | [Messaging](./part6/ch31-02-messaging.md) |
| `id_effect_events` | integration | Event store, projections, snapshots, CQRS dispatch | [Events and projections](./part5/ch23-00-events-and-projections.md) |
| `id_effect_opentelemetry` | integration | OpenTelemetry span hook, metrics bridge, logs | [Observability](./part6/ch27-00-observability.md) |
| `id_effect_cli` | integration | `run_main`, `Exit`/`Cause` → `ExitCode` | [CLI with clap](./part3/ch16-00-cli-with-clap.md) |
| `id_effect_fp` | maintenance | algebra, persistent collections, STM | [Part V](./part5/ch17-00-algebra-type-classes.md), [STM](./part4/ch12-00-stm.md) |
| `id_effect_optics` | maintenance | Lenses, prisms, traversals | [Optics](./part5/ch18-00-optics.md) |
| `id_effect_rpc`, `id_effect_ai`, `id_effect_parse`, `id_effect_lint`, `id_effect_workflow` | labs (unpublished) | Experimental; path/git dependency on `labs/`, built with `--manifest-path labs/Cargo.toml` | Part VI chapters marked "labs" |

For a single local index, run `cargo doc --workspace --no-deps` from the repository root (see each crate's `README` for optional examples). Labs crates: `cargo doc --manifest-path labs/Cargo.toml --workspace --no-deps`.
