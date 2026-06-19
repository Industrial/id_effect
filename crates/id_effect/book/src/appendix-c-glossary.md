# Glossary

Key terms used throughout this book, in alphabetical order.

---

**`~` (bind operator)**
The prefix operator inside `effect!` that runs an inner effect and binds its success value to a variable. `let x = ~eff` runs `eff` and assigns the result to `x`; `~eff` runs `eff` and discards the result.

---

**Backpressure**
The mechanism by which a slow consumer signals to a fast producer to slow down or drop data. In id_effect, expressed via `BackpressurePolicy`: `Block`, `DropLatest`, `DropOldest`, or `Unbounded`. See [Backpressure Policies](./part4/ch13-03-backpressure.md).

---

**Brand**
A zero-cost newtype wrapper that creates a distinct type from a primitive. `Brand<String, EmailMarker>` and `Brand<String, NameMarker>` are different types even though both wrap `String`, preventing accidental mixing. See [Validation and Refinement](./part4/ch14-03-validation.md).

---

**`Cause<E>`**
The reason an effect failed. Three variants: `Cause::Fail(E)` (expected error), `Cause::Die(Box<dyn Any>)` (panic or defect), `Cause::Interrupt` (cancelled). See [Exit](./part3/ch08-02-exit.md).

---

**`Chunk<A>`**
A contiguous, reference-counted batch of `A` values. The unit of data in `Stream` pipelines. Cheap to clone; efficient to process in bulk. See [Chunks](./part4/ch13-02-chunks.md).

---

**Clock**
A trait abstracting time. `LiveClock` uses real system time; `TestClock` advances only when told to. Inject `Clock` through the environment so scheduling logic is testable. See [Clock Injection](./part3/ch11-04-clock-injection.md).

---

**`commit`**
The function that lifts a `Stm<A>` into an `Effect<A, Never, ()>`. Executing the effect runs the STM transaction and retries on conflict. See [Stm and commit](./part4/ch12-03-stm-commit.md).

---

**`Env`**
The runtime capability map. Values are keyed by capability keys (`DatabaseKey`, `LoggerKey`, …) and looked up in O(1). Built with `build_env([provide!(…), …])`, `run_with`, or manual `Env::insert`. See [The Environment](./part2/ch05-03-context-hlists.md).

---

**`Effect<A, E, R>`**
The central type. A description of a computation that: succeeds with a value of type `A`, can fail with a typed error of type `E`, and requires environment `R`. Effects are lazy: nothing runs until you call a runtime function. See [What Even Is an Effect?](./part1/ch01-02-what-is-an-effect.md).

---

**`effect!` macro**
The do-notation macro for writing effect programs. Converts `~expr` into flat bind chains so you can write sequential effect code without nested closures. See [The effect! Macro](./part1/ch03-00-effect-macro.md).

---

**`Exit<A, E>`**
The result of running an effect: `Exit::Success(A)` or `Exit::Failure(Cause<E>)`. Returned by `run_test` and accessible via `FiberHandle::join`. See [Exit](./part3/ch08-02-exit.md).

---

**Fiber**
A lightweight, independently-scheduled unit of concurrent work. Fibers are cheaper than OS threads and support structured cancellation. Spawn with `run_fork`; join with `handle.join()`. See [What Are Fibers?](./part3/ch09-01-what-are-fibers.md).

---

**`FiberRef`**
A fiber-scoped dynamic variable. Each fiber has its own copy; changes don't leak to parent or sibling fibers. Use for request IDs, trace contexts, and other per-fiber state. See [FiberRef](./part3/ch09-04-fiberref.md).

---

**`from_async`**
A constructor that lifts an async closure into an `Effect`. Use when wrapping third-party library futures that return `Future` rather than `Effect`. See [Creating Effects](./part1/ch02-01-creating-effects.md).

---

**`caps!(K1, K2, …)`**
A macro that names the required capability set for `Effect<A, E, caps!(…)>` at compile time. At runtime this is a `CapList<(K1, K2, …)>` backed by `Env`. Prefer `caps!(…)` over bare `Env` in public APIs that need multiple capabilities. See [The R Parameter](./part2/ch04-00-r-parameter.md).

---

**`HasSchema`**
A trait that attaches a canonical `Schema<Self>` to a type. Implement it when a type should always be parsed the same way and you want schema-driven tooling to work automatically. See [Validation and Refinement](./part4/ch14-03-validation.md).

---

**`id_effect_axum`**
Workspace crate bridging **Axum** handlers to **`Effect`**: `State<R>`, `routing::*`, `execute`, JSON + schema helpers. See [Axum host](./part2/ch07-08-axum-host.md).

---

**`id_effect_cli`**
Workspace crate for **CLI entrypoints**: optional **`clap`**, [`run_main`](./part3/ch16-00-cli-with-clap.md), and mapping [`Exit`](./part3/ch08-02-exit.md) / [`Cause`](./part3/ch08-02-exit.md) to process exit codes. See [CLI with clap](./part3/ch16-00-cli-with-clap.md).

---

**`id_effect_config`**
Workspace crate for **configuration**: `Config<T>` descriptors, Figment/serde extraction, and effectful reads from a provider in `R`. See [Configuration](./part2/ch07-10-config.md).

---

**`id_effect_logger`**
Workspace crate for an injectable **`EffectLogger`** service and pluggable log backends. See [Logging](./part2/ch07-11-logger.md).

---

**`id_effect_platform`**
Workspace crate providing **`@effect/platform`-style** HTTP, filesystem, and process **traits** plus Tokio-backed implementations (`HttpClient`, `FileSystem`, `ProcessRuntime`, …). See [Platform I/O](./part2/ch07-06-platform-services.md).

---

**`id_effect_platform::http::reqwest`**
Workspace crate: **`reqwest::Client`** as a keyed service, `send` / JSON helpers, optional pools; complements portable HTTP via **`id_effect_platform`**. See [HTTP via reqwest](./part2/ch07-07-reqwest-http.md).

---

**`id_effect_tokio`**
Workspace crate: **Tokio-backed** `Runtime` integration, re-exports `run_async` / `run_blocking` / `run_fork`, and patterns for **non-`Send`** async graphs (`spawn_blocking_run_async`). See [Tokio bridge](./part2/ch07-05-tokio-bridge.md).

---

**`id_effect_tower`**
Workspace crate: **`tower::Service`** implementations over effects, with optional **concurrency limits** and **request metrics**. See [Tower service](./part2/ch07-09-tower-service.md).

---

**`id_effect_lint`**
Custom **rustc lint** crate for id_effect-specific rules; excluded from the default workspace `members` list. See [Workspace tooling](./appendix-d-workspace-tooling.md).

---

**`effect!` macro crates (`id_effect_macro`, `id_effect_proc_macro`)**
The **`effect!`** do-notation is split between a **proc-macro** crate and a **user-facing** macro crate. See [Workspace tooling](./appendix-d-workspace-tooling.md).

---

**`ProviderSpec`**
A type that declares how to build one capability value. Providers list dependencies in `requires()` and are wired with `provide!(P)`, `build_env`, or `run_with`. [`CapabilityGraph`](../../src/capability/graph.rs) plans build order. See [Providers and Wiring](./part2/ch06-01-what-is-layer.md).

---

**`Needs<K>` trait**
A bound on the environment type parameter that expresses "this computation requires capability `K`." Prefer `caps!(DatabaseKey, LoggerKey)` on the `Effect` type; use `where R: Needs<DatabaseKey>` only for generic library functions (still use `require!` inside `effect!`). See [Widening and Narrowing](./part2/ch04-03-widening-narrowing.md).

---

**`Never`**
The uninhabited type. `Effect<A, Never, R>` cannot fail with a typed error (but may still `Die` or `Interrupt`). Eliminate `Err(never)` branches with `absurd(never)`. See [Error Handling](./part3/ch08-00-error-handling.md).

---

**`ParseErrors`**
An accumulated collection of `ParseError` values, each with a path and message. Returned by `parse(schema, unknown)`. Reports all validation failures at once, not just the first. See [ParseErrors](./part4/ch14-04-parse-errors.md).

---

**`R` (environment type parameter)**
The third type parameter of `Effect<A, E, R>`. Encodes which capabilities the computation needs — often `caps!(K1, K2)` for multi-cap public APIs, or `()` when none are required. Binaries and tests supply a concrete `Env`. See [The R Parameter](./part2/ch04-00-r-parameter.md).

---

**`run_blocking`**
The synchronous effect runner. Use in `main` and integration tests where you want a blocking call. Do not call from within library functions — return `Effect` instead. See [Laziness as a Superpower](./part1/ch01-04-laziness.md).

---

**`run_test`**
The test-aware effect runner. Like `run_blocking` but also detects fiber leaks and uses deterministic scheduling. Use in all `#[test]` functions. See [run_test](./part4/ch15-01-run-test.md).

---

**Schedule**
A value describing how to space out repeated or retried operations. Combinators: `fixed`, `exponential`, `linear`, `.take(n)`, `.until(pred)`. Used with `.retry()` and `.repeat()`. See [Schedule](./part3/ch11-01-schedule.md).

---

**Schema**
A value of type `Schema<T>` that describes how to parse an `Unknown` into a `T`. Schemas are composable: build complex schemas from primitive ones. See [Schema Combinators](./part4/ch14-02-combinators.md).

---

**Scope**
A resource lifetime boundary. Finalizers registered with a `Scope` run when the scope exits, whether by success, failure, or cancellation. Use `acquire_release` for the common bracket pattern. See [Scopes and Finalizers](./part3/ch10-02-scopes-finalizers.md).

---

**`#[capability(T)]`**
An attribute on a zero-sized struct that declares a capability key and generates `StructNameKey`. Example: `#[::id_effect::capability(Arc<dyn Db>)] struct Database;` → `DatabaseKey`. See [Capability Keys](./part2/ch05-02-tags.md).

---

**Sink**
A consumer of `Stream` elements. Receives `Chunk`s via `on_chunk` and a completion signal via `on_done`. Built-in sinks: `collect`, `fold`, `for_each`, `drain`. See [Sinks](./part4/ch13-04-sinks.md).

---

**`Stm<A>`**
A transactional computation over `TRef` values. Compose with `stm!`; execute with `commit` or `atomically`. Retries automatically on conflict; aborts on `stm::fail`. See [Stm and commit](./part4/ch12-03-stm-commit.md).

---

**Stream**
A lazy, potentially infinite sequence of values of type `A`. Processes elements in `Chunk`s. Supports all the combinators of `Effect` plus streaming-specific operators like `flat_map`, `merge`, and `take_until`. See [Streams](./part4/ch13-00-streams.md).

---

**Capability key**
A zero-sized marker type (for example `DatabaseKey`) generated by `#[capability(T)]`. Each key is associated with a value type `T` and indexes into `Env` for lookup. See [Capability Keys](./part2/ch05-02-tags.md).

---

**`TestClock`**
A `Clock` implementation for tests. Starts at Unix epoch and advances only when you call `.advance(dur)` or `.set_time(t)`. Sleep effects complete instantly when the clock passes their wake time. See [TestClock](./part4/ch15-02-test-clock.md).

---

**`TRef<T>`**
A transactional cell: a mutable `T` that can be read and written inside `Stm` transactions. Multiple `TRef`s can be read and written atomically. See [TRef](./part4/ch12-02-tref.md).

---

**`Unknown`**
The type for unvalidated wire data. All external data enters your program as `Unknown` and is converted to typed values by running it through a `Schema`. See [The Unknown Type](./part4/ch14-01-unknown.md).
