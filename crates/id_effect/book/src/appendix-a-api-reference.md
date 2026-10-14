# API Quick Reference

A condensed reference for the most commonly used types and functions in id_effect. For full documentation, use `cargo doc --open -p id_effect`.

## Core Types

| Type | Description |
|------|-------------|
| `Effect<A, E, R>` | A computation that produces `A`, can fail with `E`, and requires environment `R` |
| `Stream<A, E, R>` | A sequence of `A` values that can fail with `E` and requires environment `R` |
| `Stm<A, E>` | A transactional computation that produces `A` (in `id_effect_fp` since 0.6) |
| `Exit<A, E>` | The result of running an effect: `Success(A)` or `Failure(Cause<E>)` |
| `Cause<E>` | `Fail(E)`, `Die(Box<dyn Any>)`, or `Interrupt` |
| `Env` | Runtime capability map; built with `build_env` or manual `insert` |
| `caps!(K1, K2, …)` | Typed required-capability set for `Effect<A, E, caps!(…)>` |
| `CapList<(K1, K2, …)>` | Runtime representation of a fixed capability set |
| `Chunk<A>` | A contiguous, reference-counted batch of `A` values |
| `Unknown` | Unvalidated wire data; input type for schemas |
| `ParseErrors` | Accumulated parse failures with paths |

## Capability DI

| Item | Notes |
|------|-------|
| `#[::id_effect::capability(T)] struct Name;` | Declares a capability service; generates `Name` |
| `#[derive(::id_effect::ProviderSpecDerive)]` | Derive a provider struct |
| `#[provides(Name)]` | Marks which key a provider satisfies |
| `~Name` / `require!(Name)` | Borrow a capability inside `effect!` |
| `Needs::<Name>::need(env)` | Advanced: borrow outside `effect!` (prefer `~Key` inside `effect!`) |
| `provide!(LiveProvider)` | Box a provider for wiring |
| `run_with([…], effect)` | Build env from providers and run |
| `build_env([…])` | Build an `Env` without running |
| `Env::insert::<Cap<K>>(value)` | Manual test override on an existing env |
| `mock_capability!(…)` | Generate a test `ProviderSpec` with a closure body |

## Constructors

| Function | Type | Notes |
|----------|------|-------|
| `succeed(a)` | `Effect<A, E, R>` | Always succeeds with `a` |
| `fail(e)` | `Effect<A, E, R>` | Always fails with typed error `e` |
| `pure(a)` | `Effect<A, Never, ()>` | Alias for `succeed`; `E = Never` |
| `from_async(f)` | `Effect<A, E, R>` | Lift an async closure |
| `Effect::new(f)` / `Effect::new_async(f)` | `Effect<A, E, R>` | Build from a sync / async closure over `&mut R` |
| `effect!(…)` | `Effect<A, E, R>` | Do-notation macro |
| `Stream::from_iterable(i)` | `Stream<A, (), ()>` | Lazy stream over an owned iterator (`I::IntoIter: 'static`) |
| `Stream::unfold(s, f)` / `Stream::unfold_effect(s, f)` | `Stream<A, E, R>` | Generate a stream from state, lazily |
| `Stream::from_pull(f)` | `Stream<A, E, R>` | Custom incremental source (`PullFn`) |
| `Stream::from_effect(e)` | `Stream<A, E, R>` | Single batch from an `Effect<Vec<A>, E, R>` |

## Effect Combinators

| Method | Notes |
|--------|-------|
| `.map(f)` | Transform success value |
| `.flat_map(f)` | Chain effects |
| `.map_error(f)` | Transform error |
| `.catch(f)` | Handle typed failure |
| `.catch_all(f)` | Handle any `Cause` |
| `.tap_error(f)` | Observe a failure without changing it |
| `.ensuring(fin)` / `.on_exit(f)` | Run a finalizer on every exit |
| `.zip(other)` | Run two effects, tuple result |
| `.zip_left(other)` | Run two effects, keep left |
| `.zip_right(other)` | Run two effects, keep right |
| `.void()` | Discard the success value |
| `retry(make, schedule)` | Re-run `make()` on failure per `schedule` (`retry_with_clock` for an explicit clock) |
| `repeat(make, schedule)` | Re-run `make()` on success per `schedule` (`repeat_with_clock`) |

## Concurrency

| Function/Method | Notes |
|----------------|-------|
| `run_fork(rt, f)` | Spawn a fiber |
| `handle.join()` | `Effect` that waits for the fiber |
| `handle.interrupt()` | Cancel a fiber |
| `fiber_all(handles)` | Join many fibers |
| `FiberRef::new(initial)` | Fiber-scoped dynamic variable |
| `fiber_ref.get()` | Read current fiber's value |
| `fiber_ref.set(v)` | Set current fiber's value |
| `with_fiber_id(id, f)` | Run `f` with a specific fiber id |
| `Semaphore::make(n)` / `sem.acquire_owned()` | Bound concurrency; the permit releases on drop |
| `Supervisor::attach(scope)` | Fork child scope; cancel token when child closes |
| `supervised(&sup, policy, clock, make)` | Run `make` under restart / limit / ignore policy |
| `Supervisor::spawn(rt, …)` | `run_fork` + `supervised` on a worker |

## STM (`id_effect_fp`)

Since 0.6 STM lives in the maintenance-tier `id_effect_fp` crate.

| Function | Notes |
|----------|-------|
| `TRef::make(v)` | Create a transactional cell (an `Stm`) |
| `tref.read_stm()` | Read inside a transaction |
| `tref.write_stm(v)` | Write inside a transaction |
| `tref.modify_stm(f)` / `tref.update_stm(f)` | Modify inside a transaction |
| `commit(stm)` | Lift `Stm<A>` into an `Effect` |
| `atomically(stm)` | Alias of `commit` (also returns an `Effect`) |
| `Stm::retry()` / `Stm::check(cond)` | Restart the transaction (yielding) until a condition holds |
| `Stm::fail(e)` | Abort transaction with error; nothing is written |
| `a.or_else(b)` | Run `b` when `a` retries (non-blocking try) |
| `TQueue::bounded(n)` / `TQueue::unbounded()` | Transactional FIFO queue |
| `TMap::make()` | Transactional hash map |
| `TSemaphore::make(n)` | Transactional semaphore |

## Resources

| Function | Notes |
|----------|-------|
| `acquire_release(acq, rel)` | Bracket-style resource management |
| `scope_with(f)` / `scoped(eff)` | Run with a `Scope`; finalizers run on close |
| `Pool::make(capacity, factory)` | Reusable resource pool |
| `pool.get()` | `Effect` that borrows one resource |
| `Cache::make(capacity, ttl, load)` | Cache backed by an effect |

## Scheduling

| Function | Notes |
|----------|-------|
| `Schedule::spaced(d)` | Wait `d` between attempts |
| `Schedule::exponential(base)` | Exponential backoff |
| `Schedule::recurs(n)` | At most `n` repetitions |
| `Schedule::recurs_while(p)` / `recurs_until(p)` | Stop on a predicate |
| `Schedule::forever()` | Never stop |
| `.compose(other)` | Combine two schedules |
| `.jittered()` | Randomise delays |
| `TestClock::new(start)` | Virtual clock; `advance`, `pending_sleeps` |

## Running Effects

| Function | Notes |
|----------|-------|
| `run_blocking(eff, env)` | Synchronous runner (main/binaries) |
| `run_async(eff, env)` | Async runner (tokio integration) |
| `run_with([…], eff)` | Build env from providers and run |
| `build_env([…])` | Build an `Env` from providers |
| `run_test(eff, env)` | Test harness; detects leaks |
| `run_test_with_clock(eff, env, clock)` | Test with an explicit `TestClock` |
| `testing::assert_bounded_memory(pipeline, n, max)` | Prove a stream pipeline does not buffer its upstream |

## Schema

| Function | Notes |
|----------|-------|
| `string()` | `Schema<String>` |
| `i64()` | `Schema<i64>` |
| `f64()` | `Schema<f64>` |
| `bool_()` | `Schema<bool>` |
| `optional(s)` | `Schema<Option<T>>` |
| `array(s)` | `Schema<Vec<T>>` |
| `struct_(…)` / `tuple(…)` / `union_(…)` | Composite schemas |
| `refine(s, pred, msg)` | Add a predicate constraint |
| `schema.decode_unknown(&unknown)` | Run schema; returns `Result<T, ParseError>` |
| `#[derive(Schema)]` | Generate `schema()` + `HasSchema` for a struct (`SchemaParser` is a deprecated alias) |
| `schema::serde_bridge` | `serde_json::Value` → `Unknown` (feature `schema-serde`) |

## Macros

| Macro | Notes |
|-------|-------|
| `effect!(…)` | Do-notation for effects; use `~expr` to bind |
| `~Key` / `require!(Key)` | Borrow a capability inside `effect!` (alias) |
| `caps!(K1, K2, …)` | Typed capability set for `Effect<_, _, caps!(…)>` |
| `provide!(Provider)` | Box a provider for wiring |
| `mock_capability!(…)` | Generate a test provider |
| `pipe!(v, f, g, …)` | Pipeline for pure values |

## Workspace crates (beyond `id_effect`)

Crates are grouped by tier ([ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md)). Everything except labs is published to crates.io in lockstep.

| Crate | Tier | Role | Book |
|-------|------|------|------|
| `id_effect` | core | `Effect`, `Stream`, capabilities, fibers, schema, testing | Parts I–IV |
| `id_effect_macro` / `id_effect_proc_macro` | core | `effect!` and capability DI macros, derives | [Workspace tooling](./appendix-d-workspace-tooling.md) |
| `id_effect_tokio` | core | Tokio `Runtime`, `run_async` wiring, `spawn_blocking_run_async` | [Tokio bridge](./part2/ch07-05-tokio-bridge.md) |
| `id_effect_config` | core | Config descriptors, Figment, provider in `R` | [Configuration](./part2/ch07-10-config.md) |
| `id_effect_logger` | core | Injectable `EffectLogger` | [Logging](./part2/ch07-11-logger.md) |
| `id_effect_resilience` | core | Circuit breaker, rate limiter, bulkhead, `retry_with_breaker`, hedging | [Runtime resilience](./part5/ch21-00-runtime-resilience.md) |
| `id_effect_process` | runtime | OTP-style processes, links, monitors, supervision trees | [Processes and supervision](./part7/ch37-00-processes-supervision.md) |
| `id_effect_node` | runtime | Distribution: membership, remote processes, placement | [Distribution](./part7/ch38-00-distribution.md) |
| `id_effect_fsm` | runtime | State machines, `#[derive(Fsm)]`, `FsmProcess`, sagas | [State machines](./part5/ch19-00-state-machines.md) |
| `id_effect_platform` | integration | HTTP / FS / subprocess (`command`) capabilities; `auth` feature | [Platform I/O](./part2/ch07-06-platform-services.md) |
| `id_effect_axum` | integration | Axum host, `EffectExecutor`; `tower` feature for `service` | [Axum host](./part2/ch07-08-axum-host.md) |
| `id_effect_sql` | integration | `SqlClient`, transactions; `pg` feature for Postgres | [Data access](./part6/ch28-00-data.md) |
| `id_effect_opentelemetry` | integration | OpenTelemetry tracing layers | [Observability](./part6/ch27-00-observability.md) |
| `id_effect_cli` | integration | `run_main`, `Exit`/`Cause` → `ExitCode` | [CLI with clap](./part3/ch16-00-cli-with-clap.md) |
| `id_effect_fp` | maintenance | algebra, persistent collections, STM | [Part V](./part5/ch17-00-algebra-type-classes.md), [STM](./part4/ch12-00-stm.md) |
| `id_effect_optics` | maintenance | Lenses, prisms, traversals | [Optics](./part5/ch18-00-optics.md) |
| `id_effect_tower`, `id_effect_sql_pg` | shim (0.6 only) | Deprecated re-exports of `id_effect_axum::service` / `id_effect_sql::pg` | — |
| `id_effect_rpc`, `id_effect_ai`, `id_effect_parse`, `id_effect_lint`, `id_effect_workflow`, `id_effect_events`, `id_effect_jobs`, `id_effect_graph` | labs (unpublished) | Experimental; path/git dependency on `labs/`, built with `--manifest-path labs/Cargo.toml` | Part VI chapters marked "labs" |

For a single local index, run `cargo doc --workspace --no-deps` from the repository root (see each crate's `README` for optional examples). Labs crates: `cargo doc --manifest-path labs/Cargo.toml --workspace --no-deps`.
