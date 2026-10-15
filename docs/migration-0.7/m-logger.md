# `id_effect_logger`: effects on the 0.7 kernel

0.7 moves `id_effect_logger` to the new kernel. This is a kernel migration
only (R7): the thread-local install API is unchanged. The providers
(`provide_minimum_log_level`, `LogMetadataLive`, `provide_composite_logger`, `EffectLoggerLive`)
keep their names but are `Layer`s now (see the last section), and the backends (`LogBackend`, `Logger`,
`CompositeLogBackend`, `JsonLogBackend`, `StructuredLogBackend`, `TracingLogBackend`) are
untouched. The public signatures differ only in their kernel types:

| item | 0.6 | 0.7 |
|---|---|---|
| `EffectLogger::{log, log_string, trace, debug, info, warn, error, fatal}` | v1 `Effect<(), EffectLoggerError>` (`R = ()`) | `Effect<(), EffectLoggerError>` (`R = HNil`) |
| `annotate_logs`, `with_log_span` | v1 `Effect<A, E, R>`, `R: Send` | `Effect<A, E, R>`, `R: Send + Sync` |
| `EffectLogger::with_minimum_log_level` | v1 `FiberRef<LogLevel>` and v1 `Effect<B, E, R>` | `FiberRef<LogLevel>` and `Effect<B, E, R>` (`B, E: Send`, `R: Send + Sync`) |
| `EffectLogMinLevel` | `= v1 FiberRef<LogLevel>` | `= FiberRef<LogLevel>` |

```rust
// 0.6
fn program() -> Effect<u32, EffectLoggerError, caps!(EffectLogger)> {
  effect!(|r| {
    let logger = *~EffectLogger;
    ~logger.info("all good");
    7
  })
}
assert_eq!(run_with([provide!(EffectLoggerLive)], program()).unwrap(), 7);
run_blocking(EffectLogger.warn("plain value"), ()).unwrap();

// 0.7
use id_effect::{Effect, caps, effect};
fn program() -> Effect<u32, EffectLoggerError, caps!(EffectLogger)> {
  effect!({
    let logger = ~EffectLogger; // `&Arc<EffectLogger>`
    ~logger.info("all good");
    7
  })
}
assert_eq!(program().provide(EffectLogger).block_on(), Exit::Success(7));
assert_eq!(EffectLogger.warn("plain value").block_on(), Exit::Success(()));
```

- **Lifting a log step** into a combinator chain over another environment: `.into_env()` is
  now `.narrow()`. Inside `effect!`, `~logger.info(..)` binds in any environment as before.
- **An unmigrated (v1) caller** wraps a log step with the bridge:
  `use id_effect::compat::IntoV1; logger.info("..").into_v1::<R>()`.
- **The minimum level is a `FiberRef`.** Its value lives in the running fiber, so
  `fr.set(level)` affects the rest of the same program (and fibers it forks afterwards), not a
  later `block_on`: each run edge starts a root fiber from the ref's initial value (the level
  given to `provide_minimum_log_level`). In 0.6 a `run_blocking(fr.set(..), ())` call leaked into
  later runs on the thread. Set the level inside the program, or scope it with
  `EffectLogger::with_minimum_log_level(fr, level, inner)`:

  ```rust
  let HCons(fr, HNil) = provide_minimum_log_level::<Never>(LogLevel::Trace)
    .build().block_on().into_result().expect("infallible");
  fr.set(LogLevel::Warn).then(EffectLogger.info("dropped")).block_on();
  ```
- **Thread-locals are read on the thread that runs the step.** As in 0.6, the pipeline is
  installed per thread. The blocking edge (`Effect::block_on`) drives the root fiber on the
  calling thread, so a program run there sees what that thread installed. Fibers that run on
  executor worker threads (`fork`, `Runtime::run` on tokio) see only what those threads
  installed; without an installed composite backend they fall back to `tracing`.
- Annotations and the span stack follow `FiberRef` inheritance: a forked child starts
  with a copy of its parent's annotations and spans.
- The providers no longer call `run_blocking` to allocate their fiber refs (`FiberRef::new`
  is synchronous), so building them is non-blocking.

## The providers are `Layer`s (k-retire-v1)

With the v1 capability DI retired, the logger providers are `id_effect::Layer`s. The
names are unchanged; `build_env([...])` / `provide!(..)` / `ProviderBox` are gone. Each layer
needs nothing (`In = HNil`), never fails (any `E`), and installs its thread-local state when it
is **built**, on the thread that drives the build. The private thread-local install functions
behind them are unchanged (R7).

| 0.6 / early 0.7 | 0.7 |
|---|---|
| `provide!(EffectLoggerLive)` | `EffectLoggerLive::layer()` → `Layer<HCons<EffectLogger, HNil>, E>` |
| `provide!(LogMetadataLive)` | `LogMetadataLive::layer()` → `Layer<HCons<EffectLogMetadata, HNil>, E>` |
| `provide_composite_logger(c) -> ProviderBox` | `provide_composite_logger(c)` → `Layer<HCons<EffectLogComposite, HNil>, E>` |
| `provide_minimum_log_level(l) -> ProviderBox` | `provide_minimum_log_level(l)` → `Layer<HCons<EffectLogMinLevel, HNil>, E>` |
| `Needs::<EffectLogMinLevel>::need(&env).clone()` | the `FiberRef` in the built layer's output |
| `EffectLoggerLive`, `LogMetadataLive` derive `ProviderSpec` | plain `Copy` unit structs with a `layer()` |

```rust
// before
let env = build_env([
  provide!(LogMetadataLive),
  provide_composite_logger(comp),
  provide_minimum_log_level(LogLevel::Info),
])?;
let fr = Needs::<EffectLogMinLevel>::need(&env).clone();

// 0.7: build with the program ...
let pipeline = LogMetadataLive::layer()
  .merge(provide_composite_logger(comp))
  .merge(provide_minimum_log_level(LogLevel::Info));
let exit = program.provide_layer(pipeline).block_on();

// ... or up front, keeping the min-level ref
let HCons(fr, HNil) = provide_minimum_log_level::<Never>(LogLevel::Info)
  .build()
  .block_on()
  .into_result()
  .expect("infallible");
```

`Layer::effect` re-runs the install on every build, so each `provide_layer` run installs a fresh
minimum-level ref (and fresh annotation/span refs) on the running thread, like rebuilding the
0.6 providers did.
