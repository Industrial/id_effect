# id_effect 0.7.0 — planning brief

Branch `release/0.7.0`, worktree `.tmp/worktrees/release-0.7.0` (base: v0.6.0, 68d4b02).
Theme: **the runtime release**. Shipped as ONE 0.7.0. Labs (`labs/`) untouched.

## Decision record (user-approved in RESEARCH, 2026-10-14)

| # | Decision |
|---|---|
| D1 | Driver: the library itself. North star: **Effect.ts parity**. |
| D2 | One big 0.7.0. Breaking changes allowed; migration guide required. |
| D3 | Own fiber interpreter loop on an `Executor` trait; tokio = default backend. Real wakers, cooperative interrupt points, parent/child fiber trees (Effect.ts FiberRuntime analogue). |
| D4 | `Effect<A,E,R>` is **Send by default**; `LocalEffect` (or equivalent) opt-in for !Send. |
| D5 | **Typed R**: capability presence checked at compile time (`need` can never panic). Two competing designs (HList+index inference vs trait-bound env); designed TOGETHER with D3/D4 so the kernel signature breaks once. |
| D6 | **Compute Fabric integrates as the scheduler** behind `zip_par` / `all{concurrency}` (ADR 0007 Phase C amendment). |
| D7 | Parity layer: composable `Layer` values (merge/provide/fresh/scoped), typed `Schedule<Out,In>` + retry/repeat as Effect methods, `Effect.request` with auto batching/caching, small wins (Random/TestRandom, `Cause::Die` keeps panic payload, metrics registry w/ bucketed histograms, Schema encode/transform, `Config<A>` descriptor). |
| D8 | Prune: tower + sql_pg shims, deprecated exports (ComputeSupervisor, SchemaParser), `StreamV1`, `#[capability]` no-op, unused root re-exports (pipe1/2/3, memoize, flip/const_/tupled/untupled, MutableRef), back-compat module aliases. **Keep** id_effect_fp, id_effect_optics. |
| D9 | Benchmarks vs 0.6 baseline. bind/map hot path may regress **≤ 25%**; fiber sleep, spawn, axum request path must **improve**. Failing spike gate → ship anyway within budget. |
| D10 | Release gate: showcase HTTP service (axum + sql_pg + config + otel) green in CI incl. integration tests; bench report vs 0.6; 0.6→0.7 migration guide; existing gates (tests, coverage ≥ 96%, tier check, docs, audit, MSRV). |
| — | Fix regardless: `crates/id_effect/src/coordination/request_resolver.rs:102` runs `run_blocking(fetch).expect` inside async → panics + blocks executor. |
| Process | Small commits on `release/0.7.0`. Full subagent concurrency in EXECUTE. Maestro tracks tasks (claims `--skip-worktree`). |

## Research evidence (core audit, file:line)

Root cause: `Effect` is a non-Send boxed future (`crates/id_effect/src/kernel/effect.rs:73`) driven by busy-polling
`run_blocking` with `Waker::noop` + `thread::yield_now` (`crates/id_effect/src/runtime/execute.rs:62-79`).
- `TokioRuntime::spawn_with` drives fibers via `run_blocking` on blocking pool (`crates/id_effect_tokio/src/lib.rs:191-195`); workarounds: `SpinClock` (`crates/id_effect_process/src/clock_util.rs:4-9`), `crates/id_effect_process/src/node.rs:4-25`. `run_blocking(` in 28 core src files.
- Interrupt only observes: `FiberHandle::interrupt` fails a Deferred (`concurrency/fiber_handle.rs:92-94`); job keeps running (`runtime/rt.rs:110-113`); `parent` arg ignored (`rt.rs:34,43`).
- One `Box<dyn EffectOp>` + `Box::pin` per combinator (`effect.rs:215,272-315`); ready Result bind boxes (`effect.rs:118-120`).
- Streams: `Arc<Mutex<StreamState>>`, Box::pin + lock per pull (`streaming/stream.rs:223,418-429`); `map_effect` clones R per element (`stream.rs:1185`); `StreamSender::fail` spins (`stream.rs:206-211`); ADR 0008 Amd 1 says map_effect serial but code is `buffer_unordered` (`stream.rs:1193`).
- `join_binds*` use join not try_join (`kernel/par_bind.rs:88-99`), arity ≤4, error shapes differ (`par_bind.rs:40-57`).
- Fiber A/E must be Clone+Send+Sync because Deferred is `watch`-based (`coordination/deferred.rs:18-27`, `rt.rs:27-28`).
- Pure constructors return Effect (Deferred/Semaphore/Bulkhead/Cache::make) → `run_blocking(..).expect` noise.
- Cache LRU touch O(n) (`resource/cache.rs:75-82`); metrics `Arc<Mutex<u64>>`, unbounded histogram Vec (`observability/metric.rs:18-42`); STM global lock + yield spin (`id_effect_fp/src/stm/mod.rs:9-28`).
- Free-fn retry*/repeat* (`scheduling/schedule.rs:298-549`); untyped Schedule enum with `&mut self` state (`schedule.rs:47,215`); two Duration types (axum uses core, resilience std).
- Logger installs globals via thread-locals (`id_effect_logger/src/lib.rs:76-125`); config fn explosion (`id_effect_config/src/lib.rs:123-218`); `ProviderSpec::on_shutdown` sync (`capability/provider.rs:48,70`); `effect!` token-level `>>` limitation (`id_effect_proc_macro/src/transform.rs:39-42`).
- R not statically checked: `HasCap<K>` blanket impl for any `Deref<Target=Env>` (`capability/set.rs:183`); `need` panics on missing cap (`capability/needs.rs:40-49,70`); `project_at_N` hand-unrolled to 8. ADR 0003:21 deferred const-generic arrays.
- No `race/timeout/zip_par/all{concurrency}/fork_daemon` anywhere; only `hedged` in resilience (`id_effect_resilience/src/hedged.rs:11-28`).
- `Cause::Die(String)` loses panic payload (`failure/cause.rs:15-26`). No Random service. Schema parse-only, struct ≤4 (`schema/mod.rs:43-46`).
- Unknowns: panic handling (`catch_unwind`) in core runtimes; no benchmarks were run.
- Deferred ADR items: optional tokio (`docs/adrs/0010-crate-tiers.md:89-93`), cache structural sharing (0010:94-96), ADR 0007 Phase C `Effect::par_all` never built.

Crate sizes (src LOC): id_effect 31.7k, fp 13.5k, node 7.2k, proc_macro 4.9k, axum 4.0k, config 3.5k, platform 2.7k, process 2.7k, optics 2.3k, otel 1.8k, fsm 1.6k, logger 1.6k, resilience 1.0k, sql 1.0k, cli 0.6k, tokio 0.4k, macro 0.3k, tower/sql_pg 8-line shims.
