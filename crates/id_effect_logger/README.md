# id_effect_logger

`EffectLogger` capability for [`id_effect`](https://github.com/Industrial/id_effect): logging as
effectful steps, forwarded to [`tracing`](https://docs.rs/tracing) by default.

- `EffectLogger` methods (`trace` / `debug` / `info` / `warn` / `error` / `fatal` / `log`) each return an
  `Effect<(), EffectLoggerError>` with no environment, so log lines compose with the rest of a
  program: `~logger.info("…")` binds inside any `effect!` (no turbofish), and
  `.into_env()` lifts a log step into a combinator chain over another environment.
- Provide it with `provide!(EffectLoggerLive)` and extract it inside `effect!` with
  `~EffectLogger`, or use it directly as a value.
- Fiber-scoped minimum level (`with_minimum_log_level`), annotations (`annotate_logs`) and log spans
  (`with_log_span`).
- Pluggable backends: `TracingLogBackend`, `JsonLogBackend`, `StructuredLogBackend` and
  `CompositeLogBackend` (see `provide_composite_logger`).
- Messages accept `impl Into<Cow<'static, str>>`: literals stay zero-copy.

## Quick start

```rust
use id_effect::{Effect, caps, effect, provide, run_blocking, run_with};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

// As a plain value:
run_blocking(EffectLogger.info("service starting"), ()).unwrap();

// As a capability inside `effect!`:
let prog: Effect<u32, EffectLoggerError, caps!(EffectLogger)> = effect!(|r| {
  let logger = *~EffectLogger;
  ~logger.info("handling request");
  42
});
assert_eq!(run_with([provide!(EffectLoggerLive)], prog).unwrap(), 42);
```

Install a `tracing` subscriber (e.g. `tracing_subscriber::fmt().init()`) to see the output.

## Examples

| Example | Shows |
|---------|-------|
| `effect_logger` | every level on `EffectLogger` as a plain value |
| `context_service` / `layer_build` | providing `EffectLoggerLive` and extracting it with `~EffectLogger` |
| `log_effects` | all five levels as `~logger.level(…)` steps |
| `macro_sync_bind` / `macro_block_tail` | mixing log steps with other binds in `effect!` |

Run one with `cargo run -p id_effect_logger --example log_effects`.
