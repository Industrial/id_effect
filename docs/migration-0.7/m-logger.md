# `id_effect_logger`: effects on the 0.7 kernel

0.7 moves `id_effect_logger` to the new kernel (`id_effect::next`). This is a kernel migration
only (R7): the thread-local install API is unchanged. You still build the providers
(`provide_minimum_log_level`, `LogMetadataLive`, `provide_composite_logger`, `EffectLoggerLive`)
with `build_env`, they still return `ProviderBox`es, and the backends (`LogBackend`, `Logger`,
`CompositeLogBackend`, `JsonLogBackend`, `StructuredLogBackend`, `TracingLogBackend`) are
untouched. The public signatures differ only in their kernel types:

| item | 0.6 | 0.7 |
|---|---|---|
| `EffectLogger::{log, log_string, trace, debug, info, warn, error, fatal}` | v1 `Effect<(), EffectLoggerError>` (`R = ()`) | `next::Effect<(), EffectLoggerError>` (`R = HNil`) |
| `annotate_logs`, `with_log_span` | v1 `Effect<A, E, R>`, `R: Send` | `next::Effect<A, E, R>`, `R: Send + Sync` |
| `EffectLogger::with_minimum_log_level` | v1 `FiberRef<LogLevel>` and v1 `Effect<B, E, R>` | `next::FiberRef<LogLevel>` and `next::Effect<B, E, R>` (`B, E: Send`, `R: Send + Sync`) |
| `EffectLogMinLevel` | `= v1 FiberRef<LogLevel>` | `= next::FiberRef<LogLevel>` |

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
use id_effect::next::{Effect, caps_next, effect_next};
fn program() -> Effect<u32, EffectLoggerError, caps_next!(EffectLogger)> {
  effect_next!({
    let logger = ~EffectLogger; // `&Arc<EffectLogger>`
    ~logger.info("all good");
    7
  })
}
assert_eq!(program().provide(EffectLogger).block_on(), Exit::Success(7));
assert_eq!(EffectLogger.warn("plain value").block_on(), Exit::Success(()));
```

- **Lifting a log step** into a combinator chain over another environment: `.into_env()` is
  now `.narrow()`. Inside `effect_next!`, `~logger.info(..)` binds in any environment as before.
- **An unmigrated (v1) caller** wraps a log step with the bridge:
  `use id_effect::next::compat::IntoV1; logger.info("..").into_v1::<R>()`.
- **The minimum level is a `next` `FiberRef`.** Its value lives in the running fiber, so
  `fr.set(level)` affects the rest of the same program (and fibers it forks afterwards), not a
  later `block_on`: each run edge starts a root fiber from the ref's initial value (the level
  given to `provide_minimum_log_level`). In 0.6 a `run_blocking(fr.set(..), ())` call leaked into
  later runs on the thread. Set the level inside the program, or scope it with
  `EffectLogger::with_minimum_log_level(fr, level, inner)`:

  ```rust
  let fr = Needs::<EffectLogMinLevel>::need(&env).clone();
  fr.set(LogLevel::Warn).then(EffectLogger.info("dropped")).block_on();
  ```
- **Thread-locals are read on the thread that runs the step.** As in 0.6, the pipeline is
  installed per thread. The blocking edge (`Effect::block_on`) drives the root fiber on the
  calling thread, so a program run there sees what that thread installed. Fibers that run on
  executor worker threads (`fork`, `Runtime::run` on tokio) see only what those threads
  installed; without an installed composite backend they fall back to `tracing`.
- Annotations and the span stack follow `next` `FiberRef` inheritance: a forked child starts
  with a copy of its parent's annotations and spans.
- The providers no longer call `run_blocking` to allocate their fiber refs (`FiberRef::new`
  is synchronous), so building them is non-blocking.
