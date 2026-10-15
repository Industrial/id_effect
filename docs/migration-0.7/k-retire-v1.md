# Retiring the v1 kernel, part 1: tracing buffer, moved shared types, `id-effect-diagnose` removed

## In-process tracing buffer on the `next` kernel

The 0.6 tracing buffer (`install_tracing_layer`, `snapshot_tracing`, `EffectEvent`, …) now lives
in `id_effect::next` (`next::observability::tracing`) and is fed by a span hook instead of a v1
`with_span` wrapper:

```rust
// 0.6
use id_effect::{TracingConfig, install_tracing_layer, run_blocking, snapshot_tracing, with_span};
run_blocking(install_tracing_layer(TracingConfig::enabled()), ()).unwrap();
run_blocking(with_span(program, "request"), env);
let snap = snapshot_tracing();

// 0.7
use id_effect::next::{TracingConfig, install_tracing_layer, snapshot_tracing, tracing_hook};
install_tracing_layer(TracingConfig::enabled()); // a plain function now
let exit = program.with_span("request").with_span_hook(tracing_hook()).block_on();
let snap = snapshot_tracing();
```

- `install_tracing_layer` is a synchronous function (it only flips the switch and clears the
  buffer).
- Spans are recorded only under the `TracingHook` span hook (`Effect::with_span_hook` /
  `LocalEffect::with_span_hook`, inherited by forked children). A span's attributes
  (`with_span_attrs`) seed its `SpanRecord` annotations. A defect or an interrupt ends a span
  with `EffectEvent::Failure`, like a typed failure.
- `annotate_current_span(key, value)` is an `Effect<(), E, R>` for any `E`/`R` (no turbofish, no
  sealed success/error traits). `emit_effect_event` / `emit_fiber_event` / `emit_compute_event`
  are effect steps; `record_*` are their synchronous forms.
- The 0.6 API keeps working until the old kernel is deleted: it records into the same buffer.
- `id_effect_cli::RunMainConfig::with_tracing()` uses the new buffer: it installs it and runs the
  program under `tracing_hook()`, so the program's `with_span`s show up in `snapshot_tracing()`.

## Kernel-independent types moved

| Type | 0.6 home | 0.7 home | old path |
|---|---|---|---|
| `FiberId` | `concurrency::fiber_id` | `failure::fiber_id` (root `id_effect::FiberId`) | still re-exported |
| `Never` | `runtime::execute` | `failure::never` (root `id_effect::Never`) | still re-exported |
| `Chunk` | `streaming::chunk` | `next::streaming::chunk` | still re-exported |
| `Transducer`, `Reducer`, `map`/`filter`/`take` | `streaming::transducer` | `next::streaming::transducer` | still re-exported |

Import them from the crate root (`id_effect::{FiberId, Never}`) or from `id_effect::next`; the
`runtime`, `concurrency` and `streaming` paths go away with the old kernel.

## `id-effect-diagnose` removed

The `id-effect-diagnose` binary of `id_effect_cli` printed diagnostics for the 0.6 runtime
capability graph (`CapabilityGraph`, `ProviderSpec`, `plan_topological`). In 0.7 a missing or
cyclic capability is a compile error (typed `R` and `Layer`s), so the tool has nothing to check
and is gone, together with `id_effect_cli`'s `serde`, `serde_json` and `toml` dependencies.

## `examples/cli-minimal`

The template is a native `next` program now: `app_effect` returns an
`id_effect::next::Effect<(), ConfigError, ConfigEnv>` and its `ConfigEnv`, and `main` calls
`run_main(eff, env, RunMainConfig::with_tracing())` with no bridge. `load_token_secret` returns
`Result<Secret<String>, Cause<ConfigError>>`. The cookbook example (`153_cookbook_cli_edge`)
runs over an explicit capability list (`HCons<ConfigProviderService, HCons<ConsoleService,
HNil>>`, `ConsoleService` a newtype over `Arc<dyn Console>`) instead of a v1 `Env`.

# Retiring the v1 kernel, part 2a: the last v1 users outside the core crate

## Providers are `next::Layer`s

The remaining v1 capability-DI providers (`ProviderBox` / `ProviderNode` / `ProviderSpec`) outside
`id_effect` are `next::Layer`s now. Names and arguments are unchanged; each layer needs nothing
(`In = HNil`) and never fails, so its `E` is the error type of the graph it joins.

| crate | 0.6 | 0.7 | details |
|---|---|---|---|
| `id_effect_logger` | `provide!(EffectLoggerLive)`, `provide!(LogMetadataLive)` | `EffectLoggerLive::layer()`, `LogMetadataLive::layer()` | `m-logger.md` |
| `id_effect_logger` | `provide_composite_logger(c)`, `provide_minimum_log_level(l)` → `ProviderBox` | the same → `Layer<HCons<_, HNil>, E>` | `m-logger.md` |
| `id_effect_events` (`es-entity`) | `provide_es_entity_events*(..)` → `ProviderBox` | → `Layer<EventJournalEnv, E>` | `l-labs-core.md` |

The logger layers install the thread-local pipeline when they are built (the private
`install_*` functions are unchanged, R7).

## The v1 capability-DI macros of `id_effect_macro`

`caps!`, `provide!`, `providers!`, `require!` and `mock_capability!` expand to v1 capability types
(`Env`, `CapList`, `Cap`, `ProviderBox`, `ProviderSpec`, `Needs`). They are **dropped** with the v1
kernel; `pipe!` and `err!` are kernel-neutral and stay.

| macro | ruling | 0.7 replacement |
|---|---|---|
| `caps!` | dropped as a `macro_rules!`; the `next` form already exists as the `caps_next!` proc macro (`HCons<A, HCons<B, HNil>>`), which the part-2b root promotion may re-export under the `caps` name | `caps_next!(A, B)` |
| `provide!` | dropped: there is no `ProviderSpec`/`ProviderBox` to wrap | a `next::Layer` (`P::layer()`, `Layer::succeed(hlist![..])`) |
| `providers!` | dropped: a bundle of layers is `Layer::merge` | `P::layer().merge(Q::layer())` |
| `mock_capability!` | dropped: a mock is a plain value in the typed environment | `.provide(mock)` / `.provide_env(hlist![..])` / `Layer::succeed(hlist![mock])` |
| `require!` | dropped: only v1 `effect!` understood it (`effect_next!` has no `require!`) | `~Key` in `effect_next!`, `need::<Key>()` elsewhere |

`id_effect` re-exports these macros at its root and in `id_effect::macros`, and its v1 modules,
tests and examples use them, so the macro definitions are deleted in part 2b together with the v1
kernel and those re-exports. Part 2a moves their tests to `id_effect_macro/tests/v1_di_macros.rs`
(deleted with them) and keeps `tests/macros.rs` and `examples/macros_tour.rs` on `pipe!` / `err!`.

## Other

- `id_effect_cli`'s migration note (`m-cli.md`) no longer describes the v1 → next bridge.
- The coverage script no longer ignores the removed `id-effect-diagnose` binary.

# Retiring the v1 kernel, part 2b: the root API is the 0.7 kernel

The 0.6 kernel is deleted and the 0.7 kernel (design A, ADR 0011) is the crate root:
`id_effect::Effect` is the `Effect<A, E = Never, R = HNil>` that lived at `id_effect::next::Effect`,
`id_effect::effect!` is the macro that was `effect_next!`, and so on for every item.

```rust
// 0.6
use id_effect::{Effect, effect, run_blocking, succeed};
fn older(age: i64) -> Effect<i64, String, ()> {
  effect! { let a = ~succeed::<i64, String, ()>(age + 1); a }
}
assert_eq!(run_blocking(older(36), ()), Ok(37));

// 0.7
use id_effect::{Effect, Exit, effect, succeed};
fn bump(age: i64) -> Effect<i64, String> { succeed(age + 1) }
fn older(age: i64) -> Effect<i64, String> {
  effect!({ ~bump(age) })
}
assert_eq!(older(36).block_on(), Exit::succeed(37));
```

## Paths

- Every module of the 0.7 kernel is a root module now: `kernel`, `runtime`, `capability`,
  `concurrency`, `coordination`, `resource`, `streaming`, `testing`, `scope`, `layer`, `par`,
  `race`, `random`, `request`; `scheduling` is `duration` + `datetime` (unchanged) + `Schedule`;
  `observability` is `LogLevel` (unchanged) + metrics, spans and the tracing buffer; `compute` is
  the kernel's `Fabric` (always built) + the Compute Fabric substrate (feature `compute`, as
  before). Their items are re-exported at the root.
- `id_effect::next::…` keeps compiling: `next` is a hidden alias of the crate root
  (`pub use crate::*`). `k-retire-sweep` renames the call sites to `id_effect::…` and deletes it.
- A 0.6 path that names a module which still exists now means the 0.7 module: e.g.
  `id_effect::runtime::Never` no longer resolves (use `id_effect::Never`), and
  `id_effect::kernel::*` / `id_effect::streaming::*` are the 0.7 items.

## Macros

| 0.7 root macro | was | notes |
|---|---|---|
| `effect!` | `effect_next!` | the 0.6 do-notation expansion is gone; `effect_next!` stays as an alias until `k-retire-sweep` |
| `effect_local!` | `effect_local_next!` | `effect_local_next!` stays as an alias |
| `caps!` | `caps_next!` (the 0.6 `caps!` built a `CapList`) | `HCons<A, HCons<B, HNil>>`; `caps_next!` stays as an alias |
| `derive(ProviderSpecDerive)` | the same derive | emits only the inherent `layer()`; the `impl ProviderSpec`, `|deps: &Env|` constructors and `#[named(..)]` variants are compile errors now. Name kept (ruling: no rename churn before the sweep) |
| `pipe!`, `err!`, `match_effect!`, `effect_tagged`, `EffectData`, `Schema`, `Optics` | unchanged | kernel-neutral |

`provide!`, `providers!`, `require!`, `mock_capability!` and the 0.6 `caps!` (`id_effect_macro`'s
`capability` module) are deleted, as ruled in part 2a. Macro expansions name root paths
(`::id_effect::HCons`, `::id_effect::__private::…`), not `::id_effect::next::…`.

## Removed with the v1 kernel

Items with a 0.7 namesake (`Effect`, `Stream`, `Queue`, `Channel`, `Scope`, `FiberRef`, `Pool`,
`Cache`, `Schedule`, `Runtime`, …) now resolve to it; their API is the 0.7 one (see the m-* and
c-*/p-* migration notes). Removed without a namesake:

| 0.6 item | 0.7 replacement |
|---|---|
| `run_blocking`, `run_async`, `run_fork`, `run_test`, `run_test_with_clock` | `Effect::block_on` at the edge, `Runtime::run(eff).await`, `Effect::fork` / `Runtime::fork`, `TestExecutor` |
| `ThreadSleepRuntime`, the v1 `Runtime` trait, `yield_now` | `Runtime` / `TokioExecutor` / `TestExecutor`, `yield_fiber`, `sleep` |
| `Env`, `Cap`, `CapList`, `Caps`, `Needs`, `HasCap`, `FromEnv`, `Capability*`, `CapBind*`, `CapKeys`, `CapWiden`, `NoCaps`, `cap_into_bind` | typed `R`: `HCons` / `HNil`, `need::<K>()`, `~K` in `effect!`, `Selector`, `provide` / `provide_env` / `narrow` |
| `ProviderSpec`, `ProviderBox`, `ProviderNode`, `Provider`, `ProviderError`, `CapabilityGraph`, `PlannerNode`, `PlannerPlan`, `plan_topological`, `build_env`, `run`, `run_with`, `RunError`, `with_override`, `with_fiber_and_override` | `Layer` (`merge`, `provide_to`, `provide_merge`, `provide_layer`); a missing capability is a compile error |
| `IntoBind`, `into_bind`, `pure`, `box_future`, `flatten_or`, `join_binds2/3/4`, `scope_with`, the free `scoped` / `acquire_release` | `succeed`, `from_async`, `~(a, b)` joins in `effect!`, `zip_par`, `Effect::scoped`, `acquire_use_release` |
| `CancellationToken`, `check_interrupt`, `FiberStatus`, `fiber_all`, `fiber_succeed`, `fiber_never`, `interrupt_all`, `with_fiber_id`, `RestartIntensity`, `SupervisorPolicy` | fiber interruption (`FiberHandle::interrupt`, `Effect::interruptible`), `all` / `fork_all`, the 0.7 `Supervisor` |
| `QueueChannel`, `ChannelReadError`, `BatchError`, `RequestEntry`, `FnRequestResolver`, `make_request_resolver`, `batching` | `Channel` (duplex), `Channel::to_stream`, the `request` module (`RequestResolver`, batching, request cache) |
| `Finalizer` | `Scope::add_finalizer` / `add_finalizer_exit` |
| `Clock`, `LiveClock`, `TestClock` | the executor clock (`TestExecutor` virtual time) |
| `repeat`, `repeat_n`, `retry`, `forever`, `*_with_clock`, `*_with_clock_and_interrupt`, `ScheduleDecision`, `ScheduleInput` | `Effect::repeat` / `Effect::retry` (+ `_or_else`) with `Schedule` |
| `Metric` (0.6), `metric_make`, `with_span` (free fn), `AnnotateCurrentSpanSuccess`, `AnnotateCurrentSpanErr`, `TracingFiberRefs` | the 0.7 `Metric` / `MetricRegistry`, `Effect::with_span` + `tracing_hook()`, `annotate_current_span` (part 1) |
| `testing::{snapshot, proptest, stream}` (`SnapshotAssertion`, `assert_golden*`, `assert_exit_*`, `arb_exit_*`, `assert_bounded_memory`, leak / unclosed-scope recorders) and the `proptest` cargo feature | `TestExecutor`, `Exit` assertions; a pull stream with `take(n)` on an infinite source shows bounded memory |

Kept, kernel-neutral: `failure` (`Cause`, `Exit`, `Defect`, `FiberId`, `Never`, `Or`),
`foundation`, `schema`, `scheduling::{duration, datetime}` (`Duration`, `UtcDateTime`, …),
`match_`, `compute` (Compute Fabric), `parallelism`, `observability::LogLevel`, `Chunk`,
`Transducer`. `ComputeFabric`'s blocking `acquire_blocking` backs off with a 10 µs sleep instead
of `std::thread::yield_now` (same thread-level semantics).

## Tests, examples, benches

- Tests of the deleted modules are deleted with them; their 0.7 equivalents are the `tests/next_*`
  suites. The v1 trybuild suite (`tests/ui`, `ui_compile_fail`) tested v1 DI diagnostics and is
  deleted; `tests/next_ui` is re-blessed on rustc 1.99.0 (the type paths in its expected stderr
  lose `next::`).
- Examples (`crates/id_effect/examples`), on the 0.7 kernel:
  - **Ported** (59): `001`–`013`, `015`, `018`, `037`–`039`, `040_capability_app`, `042`, `048`,
    `049`, `051`, `052`, `054`, `055`, `060`–`075` (with both `071`s), `089`–`091`, `093`–`096`,
    `100`–`104`, `120`–`122`, `151_cookbook_streaming_etl`.
  - **Unchanged** (9, kernel-neutral): `014_err_macro`, `017_cause_optional`, `083`–`088`
    (schema), `105_match_matcher`.
  - **Deleted** (20, no 0.7 equivalent or a duplicate of a ported one): `016_flat_map_union`,
    `040_runtime_trait`, `041_thread_sleep_runtime`, `043_named_variant_providers`,
    `044_cancellation_tree`, `045_check_interrupt`, `046_cancelled_waits`, `047_fiber_handle`,
    `050_interrupt_all`, `053_schedule_repeat`, `056_test_clock`, `057_repeat_with_clock`,
    `058_retry_with_clock`, `059_schedule_interrupt`, `076_stream_duplex_queue`, `092_scoped`,
    `097_run_test`, `098_run_test_clock_hygiene`, `099_snapshot_corpus`,
    `150_cookbook_retry_test_clock`; and `examples/archived/` (0.5 layer examples).
- Benches: every gate id in `docs/benchmarks/gates.toml` / `gates-k.toml` still exists. The
  `coord/*` bodies run on the 0.7 `Channel` / `Queue` (a forked producer fiber, the main fiber
  consumes); `effect_macro_bind_free` compares `effect!` with `sync`; `env_lookup` and
  `startup_run_with` (0.6 `Env` / `run_with` only, no gate ids) are deleted.
- `scripts/clippy-no-blocking/clippy.toml` (G-nb) lists the edges at their root definition paths
  (`id_effect::kernel::effect::Effect::block_on`, …); the v1 `run_blocking` entry is gone.
