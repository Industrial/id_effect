# Glossary

Key terms used throughout this book, in alphabetical order.

---

**`~` (bind operator)**
The prefix operator inside `effect!`. `let x = ~eff` runs the sub-effect `eff` on the same fiber and binds its success value (converting its error with `From`); `~Cap` (a capability type) reads that capability from `R`; `~(a, b)` runs several effects in parallel and binds a tuple.

---

**Backpressure**
The mechanism by which a slow consumer signals to a fast producer to slow down or drop data. In id_effect, a stream channel's `BackpressurePolicy`: `BoundedBlock`, `DropNewest`, `DropOldest` or `Fail`. See [Backpressure Policies](./part4/ch13-03-backpressure.md).

---

**Brand**
A zero-cost newtype wrapper that creates a distinct type from a primitive. `Brand<String, EmailMarker>` and `Brand<String, NameMarker>` are different types even though both wrap `String`, preventing accidental mixing. See [Validation and Refinement](./part4/ch14-03-validation.md).

---

**Capability**
A Rust type that names a dependency in `R` (for example `Counter`, `HttpClientService` or `EffectLogger`). The type itself is the key: `caps!(Counter)` requires it, `~Counter` reads it, `.provide(Counter(1))` supplies it. See [Capability services](./part2/ch05-02-tags.md).

---

**`caps!(A, B, …)`**
The macro that spells the environment type `HCons<A, HCons<B, HNil>>` for `Effect<_, _, caps!(A, B)>`. See [The R Parameter](./part2/ch04-00-r-parameter.md).

---

**`Cause<E>`**
The reason an effect failed: `Cause::Fail(E)` (expected error), `Cause::Die(Defect)` (panic or defect), `Cause::Interrupt(FiberId)` (interrupted), combined with `Cause::Both` (parallel) and `Cause::Then` (sequential). See [Exit](./part3/ch08-02-exit.md).

---

**`Chunk<A>`**
A contiguous batch of `A` values; the unit of data in `Stream` pipelines. See [Chunks](./part4/ch13-02-chunks.md).

---

**Clock**
Time comes from the fiber's executor: `sleep`, schedules and timeouts use it. The `TokioExecutor` uses real time; the `TestExecutor` has a virtual clock that moves only when the test calls `advance`. See [Clock Injection](./part3/ch11-04-clock-injection.md).

---

**`commit`**
The `id_effect_fp` function that lifts an `Stm<A, E>` into an `Effect`. Running the effect runs the transaction and retries it on conflict (alias `atomically`). See [Stm and commit](./part4/ch12-03-stm-commit.md).

---

**`Effect<A, E = Never, R = HNil>`**
The central type. A lazy, `Send` description of a computation that succeeds with `A`, can fail with a typed `E`, and needs the capabilities in `R`. Nothing runs until the effect reaches an edge (`block_on`, `Runtime::run`); an effect runs once. See [What Even Is an Effect?](./part1/ch01-02-what-is-an-effect.md).

---

**`effect!` macro**
The do-notation macro: an async-style body in which `~` binds sub-effects and reads capabilities, so sequential effect code needs no nested closures. `effect_local!` builds a `LocalEffect`. See [The effect! Macro](./part1/ch03-00-effect-macro.md).

---

**`effect!` macro crates (`id_effect_macro`, `id_effect_proc_macro`)**
`effect!`, `effect_local!`, `caps!` and the derives are procedural macros in `id_effect_proc_macro`; `pipe!` and `err!` are declarative macros in `id_effect_macro`. `id_effect` re-exports all of them. See [Workspace tooling](./appendix-d-workspace-tooling.md).

---

**Edge**
The place where an effect is run: `eff.block_on()` in `main`, tests and benches, `Runtime::run(eff).await` in async code, or a host such as `id_effect_cli::run_main` or an Axum handler. Library code returns effects instead of running them. See [Laziness as a Superpower](./part1/ch01-04-laziness.md).

---

**`Exit<A, E>`**
The outcome of running an effect: `Exit::Success(A)` or `Exit::Failure(Cause<E>)`. Returned by `block_on` and `Runtime::run`, and by joining a fiber. See [Exit](./part3/ch08-02-exit.md).

---

**Fiber**
A lightweight, independently scheduled unit of concurrent work. Fibers are cheaper than OS threads and support structured interruption: a parent's children are interrupted when it ends. Start one with `eff.fork()`, wait with `handle.join()`. See [What Are Fibers?](./part3/ch09-01-what-are-fibers.md).

---

**`FiberRef`**
A fiber-scoped dynamic variable. Forked fibers inherit their parent's value; changes don't leak to the parent or siblings. Use for request IDs, trace contexts and other per-fiber state. See [FiberRef](./part3/ch09-04-fiberref.md).

---

**`from_async`**
A constructor that lifts a closure returning a `Future<Output = Result<A, E>>` into an `Effect`. Use it to wrap third-party futures. See [Creating Effects](./part1/ch02-01-creating-effects.md).

---

**`HasSchema`**
A trait that attaches a canonical `Schema` to a type. Implement it (or `#[derive(Schema)]`) when a type should always be parsed the same way. See [Validation and Refinement](./part4/ch14-03-validation.md).

---

**`HCons` / `HNil` / `hlist!`**
The typed environment is an HList: `HCons<A, HCons<B, HNil>>` (spelled `caps!(A, B)`), built as a value with `hlist![a, b]`. Capabilities are found by type, so their order does not matter to `~Cap` or `need::<Cap>()`. See [R Revisited](./part2/ch04-01-r-revisited.md).

---

**`id_effect_axum`**
Workspace crate bridging **Axum** handlers to **`Effect`**: an `EffectExecutor`, routing and JSON + schema helpers, request-scoped capabilities. See [Axum host](./part2/ch07-08-axum-host.md).

---

**`id_effect_axum::service`**
**`tower::Service`** implementations over effects, behind `id_effect_axum`'s `tower` feature. See [Tower service](./part2/ch07-09-tower-service.md).

---

**`id_effect_cli`**
Workspace crate for **CLI entrypoints**: optional **`clap`**, [`run_main`](./part3/ch16-00-cli-with-clap.md), and mapping [`Exit`](./part3/ch08-02-exit.md) / [`Cause`](./part3/ch08-02-exit.md) to process exit codes. See [CLI with clap](./part3/ch16-00-cli-with-clap.md).

---

**`id_effect_config`**
Workspace crate for **configuration**: `Config<T>` descriptors, Figment/serde extraction, and effectful reads from a config provider in `R`. See [Configuration](./part2/ch07-10-config.md).

---

**`id_effect_fp`**
Maintenance-tier crate holding the functional-programming layer core does not use itself: `algebra`, persistent `collections` and `stm`. See [Algebra type classes](./part5/ch17-00-algebra-type-classes.md).

---

**`id_effect_lint`** (labs)
Custom **rustc lint** crate for id_effect-specific rules, in the labs workspace. See [Workspace tooling](./appendix-d-workspace-tooling.md).

---

**`id_effect_logger`**
Workspace crate for an injectable **`EffectLogger`** service and pluggable log backends. See [Logging](./part2/ch07-11-logger.md).

---

**`id_effect_platform`**
Workspace crate providing **`@effect/platform`-style** HTTP, file system and subprocess **traits** (`HttpClient`, `FileSystem`, `ProcessRuntime`) with Tokio-backed implementations, as capabilities (`HttpClientService`, …). See [Platform I/O](./part2/ch07-06-platform-services.md).

---

**`id_effect_platform::http::reqwest`**
The **`reqwest`** backend of `id_effect_platform`'s HTTP client. See [HTTP via reqwest](./part2/ch07-07-reqwest-http.md).

---

**`id_effect_process` / `id_effect_node`**
The Process Mesh: OTP-style local processes and supervision trees (`id_effect_process`), plus distribution, membership and cluster placement (`id_effect_node`). See [Process Mesh overview](./part7/ch36-00-process-mesh.md).

---

**`id_effect_resilience`**
Workspace crate for circuit breakers, rate limiters, bulkheads, hedged requests and `retry_with_breaker`. See [Runtime resilience](./part5/ch21-00-runtime-resilience.md).

---

**`id_effect_sql`**
Workspace crate for driver-agnostic SQL as effects (`SqlClient`, `with_transaction`, `TestSqlClient`). Postgres lives in `id_effect_sql::pg` behind the `pg` feature. See [Data access](./part6/ch28-00-data.md).

---

**`id_effect_tokio`**
Workspace crate: `TokioRuntime`, which pairs an id_effect `Runtime` with an owned or ambient Tokio runtime (`run`, `fork`, `spawn`, `block_on`). The `TokioExecutor` itself lives in core. See [Tokio bridge](./part2/ch07-05-tokio-bridge.md).

---

**Crate tier**
Every workspace crate declares a tier (`core`, `runtime`, `integration`, `maintenance`, `labs`, `merge`, `example`) in `[package.metadata.id_effect]`; `scripts/tier-conformance.sh` checks the quality bar for that tier. See [ADR 0010](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0010-crate-tiers.md).

---

**Labs**
Unpublished crates with no API guarantees, kept in the separate `labs/` Cargo workspace: `id_effect_rpc`, `id_effect_ai`, `id_effect_parse`, `id_effect_lint`, `id_effect_workflow`. Use them via path or git dependencies; build with `--manifest-path labs/Cargo.toml`.

---

**`Layer<Out, E, In>`**
A recipe that builds the capabilities `Out` (an HList) from the capabilities `In`, possibly failing with `E`. Compose with `merge`, `provide_to` and `provide_merge`; a layer is built once per build (memoised) and its scoped resources are released when the build's scope closes. `#[derive(ProviderSpecDerive)]` generates a `layer()` for a provider struct. See [What Is a Provider?](./part2/ch06-01-what-is-layer.md).

---

**`need::<T>()`**
Requests capability `T` outside `effect!`: `need::<Db>().map(|db| ..)` is an effect whose `R` contains `Db`. Inside `effect!`, write `~Db`. See [Widening and Narrowing](./part2/ch04-03-widening-narrowing.md).

---

**`Never`**
The uninhabited error type and the default `E`. `Effect<A>` (`E = Never`) cannot fail with a typed error, though it may still die or be interrupted. See [Error Handling](./part3/ch08-00-error-handling.md).

---

**`ParseErrors`**
An accumulated collection of `ParseError` values, each with a path and message, so a decode reports every validation failure at once. See [ParseErrors](./part4/ch14-04-parse-errors.md).

---

**`provide` / `provide_env` / `provide_layer`**
Supply capabilities to an effect: `.provide(v)` removes one capability from `R`, `.provide_env(hlist![..])` supplies all of them, `.provide_layer(layer)` builds a `Layer` and supplies its output. A missing capability is a compile error. See [Providing Dependencies](./part2/ch04-02-providing.md).

---

**`R` (environment type parameter)**
The third type parameter of `Effect<A, E, R>`: the capabilities the computation needs, as an HList (`caps!(K1, K2)`), `HNil` (the default) when none. See [The R Parameter](./part2/ch04-00-r-parameter.md).

---

**`Runtime`**
Owns an `Executor` and the run edges: `Runtime::global()`, `run(eff).await`, `block_on(eff)`, `fork(eff)`. `eff.block_on()` is a shortcut over the global runtime. See [Laziness as a Superpower](./part1/ch01-04-laziness.md).

---

**Schedule**
An immutable value describing how to space out repeated or retried operations: `Schedule::recurs(n)`, `spaced(d)`, `exponential(base)`, `fibonacci(one)`, combined with `intersect`, `union`, `and_then`, `jittered`, `up_to`. Used with `Effect::retry(|| eff, schedule)` and `Effect::repeat(|| eff, schedule)`. See [Schedule](./part3/ch11-01-schedule.md).

---

**Schema**
A value of type `Schema<A, I, E>` that decodes wire data into an `A` and encodes it back. Schemas are composable: build complex schemas from primitive ones. See [Schema Combinators](./part4/ch14-02-combinators.md).

---

**Scope**
A resource lifetime boundary. Finalizers registered with a `Scope` run when it closes, whether by success, failure or interruption. `Effect::scoped(|scope| ..)` opens one; `acquire_use_release` covers the common bracket pattern. See [Scopes and Finalizers](./part3/ch10-02-scopes-finalizers.md).

---

**Sink**
A consumer of a `Stream` that produces one result: `Sink::collect()`, `Sink::fold_left(init, f)`, `Sink::for_each(f)`, `Sink::drain()`, or a custom `Sink::new(driver)`; run it with `sink.run(stream)`. See [Sinks](./part4/ch13-04-sinks.md).

---

**`Stm<A, E>`**
An `id_effect_fp` transactional computation over `TRef` values. Compose with `map` / `flat_map` / `or_else`; execute with `commit` (alias `atomically`). Restarts automatically on conflict or `Stm::retry`; aborts on `Stm::fail`. See [Stm and commit](./part4/ch12-03-stm-commit.md).

---

**Stream**
A lazy, pull-based, potentially infinite sequence of values, processed in `Chunk`s. Combinators include `map`, `filter`, `take`, `take_while`, `scan`, `grouped`, `flat_map`, `map_effect_par` and `merge`; `run_collect`, `run_fold` and sinks consume it. See [Streams](./part4/ch13-00-streams.md).

---

**`TestExecutor`**
A deterministic executor for tests with a virtual clock: `run_until_stalled` runs every ready fiber, `advance(d)` moves time and fires due timers, so a test of a 10-second retry takes microseconds. See [Testing](./part4/ch15-00-testing.md).

---

**`TRef<T>`**
A transactional cell: a mutable `T` that can be read and written inside `Stm` transactions. Multiple `TRef`s can be read and written atomically. See [TRef](./part4/ch12-02-tref.md).

---

**`Unknown`**
The type for unvalidated wire data. All external data enters your program as `Unknown` and is converted to typed values by running it through a `Schema`. See [The Unknown Type](./part4/ch14-01-unknown.md).
