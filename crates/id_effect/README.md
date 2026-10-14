# `id_effect`

`Effect<A, E, R>` (sync + async), capability-first dependency injection and `pipe!` —
interpreter-style, no bundled executor. Tier: **core** ([ADR 0010](../../docs/adrs/0010-crate-tiers.md)).

## Quick start

```rust
use id_effect::{Effect, effect, fail, run, succeed};

fn parse_port(raw: &str) -> Effect<u16, String, ()> {
  match raw.parse::<u16>() {
    Ok(port) => succeed(port),
    Err(e) => fail(format!("bad port {raw:?}: {e}")),
  }
}

fn next_port(raw: &str) -> Effect<u16, String, ()> {
  let parsed = parse_port(raw);
  effect! {
    let port = ~parsed;
    port + 1
  }
}

assert_eq!(run(next_port("8080")).ok(), Some(8081));
assert!(run(next_port("http")).is_err());
```

Cargo features: `compute` (default — Compute Fabric, adaptive Rayon bulk ops, `sysinfo`
telemetry), `proptest`, `schema-serde`. `--no-default-features` drops `rayon` and `sysinfo`.
Algebraic type classes, persistent (`im`) collections and STM live in the maintenance-tier
[`id_effect_fp`](../id_effect_fp) crate.

---

## Construct catalog (`src/`)

Single-page index of modules and primary APIs under `crates/id_effect/src/`. Each line states **when** to reach for it and **how** it fits the system (lazy `Effect` graphs, compile-time `R`, boundaries at `run_*`). For stratum notes and design detail, see `src/**/README.md` where present.

---

## Crate root (`lib.rs`)

| Construct | When / how to use |
|-----------|-------------------|
| **Public re-exports** | Prefer `use id_effect::{Effect, …}` for the supported surface; module paths (`effect::kernel::…`) remain for precision and older call sites. |
| **`extern crate self as effect`** | Lets proc-macros emit `::id_effect::…` paths reliably when expanding inside this crate. |

---

## `foundation/` — pure helpers core is built on (no `Effect`, no I/O)

`unit`, `never`, `function`, `product`, `coproduct`, `isomorphism` and `option_` moved to
`id_effect_fp::foundation`.

| Module / construct | When / how to use |
|---------------------|-------------------|
| **`either`** (alias + `either::either::*`) | Use when you want **Effect.ts naming** (`right`/`left`, `map_left`, `get_or_else`, …) over inherent `Result` methods. |
| **`func`** (`identity`, `always`, `compose`, `absurd`) | Use for small function plumbing; the wider `Function` set (`pipe*`, `flip`, `tupled`, …) is in `id_effect_fp::foundation::function`. |
| **`piping`** (`Pipe` trait) | Use for fluent `x.pipe(f).pipe(g)` on any value; pairs with `Effect::map` / `pipe!` at boundaries. |
| **`predicate`** (`Predicate`, `predicate::*`) | Use when boolean logic gets gnarly: store composable `Send + Sync` predicates (`and`/`or`/`not`, `contramap`, builtins). |

---

## `kernel/` — `Effect` core

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`effect`** (`Effect`, `BoxFuture`, `IntoBind`, `succeed`/`fail`/`pure`, `from_async`, `scoped`/`scope_with`, `acquire_release`, …) | **Primary abstraction:** describe async/env work as a lazy graph; **run only** via `run_blocking` / `run_async` / test harnesses; bind inside `effect!` with `~`. |
| **`par_bind`** (`join_binds2/3/4`, `flatten_or`) | Use to run independent binds concurrently and join their results. |

The pure `thunk`, `result` and `reader` helpers moved to `id_effect_fp::{thunk, result, reader}`.

---

## `context/` — typed environment (`R`)

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`tag` / `tagged` / `tagged()`** | Use to brand values in the environment list so services are **lookup-by-type** instead of stringly maps. |
| **`hlist` (`Cons`, `Nil`) / `Context`** | Use to build the heterogeneous `R` stack for `Get`/`GetMut`; **compile-time** selection of dependencies. |
| **`path` (`Here`, `Skip*`, `There`, …)** | Use to point at a specific tagged cell in the tail when multiple tags share types or you need explicit paths. |
| **`get` (`Get`, `GetMut`)** | Use (mostly via impls) to project values out of `R` immutably or mutably. |
| **`wrapper` (`prepend_cell`, …)** | Use to extend/shrink `Context` values when assembling layers by hand (layers usually do this for you). |
| **`match_` (`Matcher`, `HasTag`)** | Use for **ordered** runtime routing (`when`/`tag`/`or_else`) on tagged values—like a typed `match` with predicates. |
| **`optics` (`EnvLens`, `focus`, `identity_lens`)** | Use when you need **first-class** environment projections (compose/widen `R`) instead of one-off `zoom_env` closures. |

---

## `layer/` — dependency injection & layer graphs

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`factory`** (`Layer`, `Stack`, `StackThen`, `LayerFn*`, `merge_all`, …) | Use to declare **how** to build one cell of `R` and **stack** layers into a full environment (recipe ≅ `Effect` without env). |
| **`graph`** (`LayerGraph`, `LayerNode`, `LayerPlan`, …) | Use when many services declare `requires`/`provides` and you need a **planner** to topo-sort construction. |
| **`service`** (`Service`, `ServiceEnv`, `service` / `service_env`, `provide_service`, …) | Use for **Effect.ts-style** service interfaces keyed by tags and implemented via layers (DI without passing concrete structs through every fn). |

---

## `macros/` — procedural + `macro_rules!` surface

| Construct | When / how to use |
|-----------|-------------------|
| **`effect!`** (proc-macro, `effect_proc_macro`) | Use for **all** non-trivial effect code: `~` bind/`~expr` discard, tail `Ok`, readable do-notation for `Effect<A,E,R>`. |
| **`pipe!`** | Use for multi-step **pure** forward pipelines when `|x| x.pipe(f)` is too heavy. |
| **`ctx!`** | Use to build typed `Context`/`Cons`/`Tagged` values with less noise. |
| **`service_def` / `service_key`** | Use to declare service tags/env keys with less boilerplate (see macro docs). |
| **`layer_graph!` / `layer_node!`** | Use to assemble layer graphs declaratively where the macros fit your wiring style. |
| **`req!` / `err!`** | Use for small helpers around requests/errors where the macro shortens repeated patterns (see macro docs). |

---

## `runtime/` — interpreters

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`execute`** (`run_blocking`, `run_async`, `Never`) | Use **`run_blocking`** for sync hosts/tests; **`run_async`** to bridge into an async runtime; **`Never`** for uninhabited error channels in runtime signatures. |
| **`rt`** (`Runtime`, `ThreadSleepRuntime`, `run_fork`, `yield_now`) | Use when you need a **sleep/yield policy**, structured forking, or embedding the effect runner in a larger scheduler. |
| **Re-exported `concurrency` types** (`FiberId`, `FiberHandle`, …) | Use via `effect::runtime::*` for legacy paths; prefer `effect::{…}` imports for new code. |

---

## `concurrency/` — fibers & cancellation

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`fiber_id`** (`FiberId`) | Use as a stable, branded id for observability and fiber-scoped state keys. |
| **`fiber_handle`** (`FiberHandle`, `FiberStatus`, `fiber_*`, `interrupt_all`, …) | Use to **spawn/join/interrupt** fibers and inspect completion—structured async tasks with typed results. |
| **`cancel`** (`CancellationToken`, `check_interrupt`) | Use for cooperative cancellation that propagates through effect loops (polling `check_interrupt`). |
| **`fiber_ref`** (`FiberRef`, `with_fiber_id`) | Use for **fiber-local** dynamic state (Effect.ts-style) without globals. |
| **`async_notify`** (internal) | Implementation detail for wait/signal between fibers—prefer public APIs built on top. |

---

## `coordination/` — communication & sync

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`deferred`** (`Deferred`) | Use for one-shot handoff of a result between fibers (promise/future-like rendezvous). |
| **`latch`** (`Latch`) | Use to wait until an event/count reaches a threshold (barrier-style). |
| **`queue`** (`Queue`, `QueueError`) | Use for fiber-safe FIFO messaging with explicit error semantics. |
| **`semaphore`** (`Semaphore`, `Permit`) | Use to bound concurrency / rate-limit sections paired with `Scope` finalization. |
| **`pubsub`** (`PubSub`) | Use for broadcast/multicast fanout between producers and subscribers. |
| **`channel`** (`Channel`, `QueueChannel`, `ChannelReadError`) | Use for stream-like or mailbox communication patterns built on queues/chunks. |
| **`ref_`** (`Ref`) | Use for shared mutable cells integrated with the effect runtime (see module docs vs raw `Rc<RefCell<_>>`). |
| **`synchronized_ref`** (`SynchronizedRef`) | Use when you need mutex-backed shared state with the coordination helpers’ lifecycle. |

---

## `failure/` — structured failure & outcomes

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`cause`** (`Cause`) | Use to represent **non-fatal** structured errors, often composable as a semigroup (accumulate warnings). |
| **`exit`** (`Exit`) | Use for **terminal** fiber outcomes pairing success/failure with final causes. |
| **`union`** (`Or`) | Use to model alternation of error shapes (`Or<L,R>`) without nested `Result` hell. |

---

## `resource/` — lifetimes & caching

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`scope`** (`Scope`, `Finalizer`) | Use to acquire resources with **deterministic finalizers** (unwind-safe cleanup in effect/fiber scopes). |
| **`pool`** (`Pool`, `KeyedPool`) | Use to reuse expensive handles (DB/clients) with lifecycle tied to scopes/pools. |
| **`cache`** (`Cache`, `CacheStats`) | Use for in-memory memoization of effectful lookups with explicit eviction/stats when appropriate. |

---

## `scheduling/` — time & policies

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`duration`** (`Duration`, `DurationParseError`) | Use for human-parseable and arithmetic durations in schedules/policies. |
| **`datetime`** (`UtcDateTime`, `ZonedDateTime`, `AnyDateTime`, `TimeUnit`, `timezone`) | Use when you need civil timestamps/time zones beyond plain monotonic clocks. |
| **`clock`** (`Clock`, `LiveClock`, `TestClock`) | Use to **inject time** for deterministic tests (`TestClock`) vs wall time (`LiveClock`). |
| **`schedule`** (`Schedule`, `Schedule*`, `repeat*`, `retry*`, `forever`) | Use to express **retry/repeat** policies and interpret them into `Effect` loops (optionally with interrupt/cancellation). |

---

## `observability/` — metrics & tracing

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`metric`** (`Metric`, `metric_make`) | Use for lightweight counters/gauges/histograms integrated with effect execution (not a full metrics backend by itself). |
| **`tracing`** (`TracingConfig`, `with_span`, `emit_*_event`, `TracingFiberRefs`, …) | Use to correlate logs/spans with fibers/effects and snapshot fiber-ref state for debugging. |

---

## `schema/` — data descriptions & parsing

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`brand`** (`Brand`, `RefinedBrand`) | Use to distinguish type-identical values (newtype branding) for safer APIs without extra runtime tags. |
| **`equal`** (`Equal`, `EffectHash`, `combine`/`equals`/`hash*`) | Use to define stable equality/hashing for structural values used in collections/schema. |
| **`data`** (`EffectData`, `DataStruct`/`DataTuple`, `DataError`) | Use as the runtime representation layer for schema-driven data (records/tuples/errors). |
| **`order`** (`Ordering`, `DynOrder`, `ordering`) | Use for deterministic sorting when you need dynamic/order witnesses beyond `Ord` alone. |
| **`parse`** (`Schema`, `ParseError`, `Unknown`, primitives, `struct_*`, `tuple*`, `union_`, `optional`, `refine`, `filter`, `transform`, …) | Use to **parse/validate** unknown wire values into typed data (core combinator library for IO boundaries). |
| **`extra`** (`record`, `suspend`, `union_chain`, `literal_*`, `null_or`, `wire_equal`, …) | Use for higher-level schema builders and JSON-like convenience (see module for exact combinators). |
| **`parse_errors`** (`ParseErrors`) | Use to accumulate multiple parse diagnostics instead of failing fast on the first error. |
| **`has_schema`** (`HasSchema`) | Use to associate a stable schema description with Rust types (derive/metadata patterns). |
| **`json_schema_export`** *(feature `schema-serde`)* | Use to export JSON-schema–like fragments for interop/documentation. |
| **`serde_bridge`** *(feature `schema-serde`)* (`unknown_from_serde_json`) | Use to bridge `serde_json::Value` into `Unknown` for schema parsing. |

---

## `streaming/` — chunked streams & sinks

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`chunk`** (`Chunk`) | Use as the incremental payload for streaming pipelines (backpressure-friendly batches). |
| **`sink`** (`Sink`) | Use to consume chunks with explicit lifecycle/error handling. |
| **`stream`** (`Stream`, `StreamSender`, `stream_from_channel*`, `BackpressurePolicy`, …) | Use to build **lazy** producers/consumers integrated with channels and policies (`send_chunk`, `end_stream`, fanout/broadcast helpers). |

---

## Moved to `id_effect_fp`

`algebra/` (type classes), `stm/` (`TRef`, `TMap`, `commit`, …) and `collections/`
(`EffectHashMap`, `EffectVector`, `RedBlackTree`, `MutableList`, `ChunkBuilder`, `Trie`, …) live in
the maintenance-tier [`id_effect_fp`](../id_effect_fp) crate. Core uses `std` collections
internally and in its public types (`SpanRecord::annotations`, `Metric::snapshot_frequencies`,
`Sink::collect_to_map`, `TimeBucketMap` are `std::collections::{HashMap, BTreeMap}`).

---

## `testing/` — harness helpers

| Module / construct | When / how to use |
|--------------------|-------------------|
| **`test_runtime`** (`run_test`, `run_test_with_clock`, leak/scope assertions) | Use in tests to execute effects deterministically and assert **fiber/scope hygiene**. |
| **`snapshot`** (`SnapshotAssertion`) | Use for structured golden/snapshot assertions across integrated strata (see module). |

---

## Cross-cutting pointers

- **Boundaries:** describe work with `Effect`; **run** with `runtime::*`; wire dependencies with `layer`/`context`/`service`; validate external inputs with `schema`.
- **Imperative sync state:** prefer `coordination`/`FiberRef` (or `id_effect_fp::stm`) over ad hoc shared mutable cells.
- **Deeper docs:** `src/kernel/README.md`, `src/context/README.md`, `src/layer/README.md`, `src/schema/README.md`, etc., expand the “why” beyond this cheat-sheet.
