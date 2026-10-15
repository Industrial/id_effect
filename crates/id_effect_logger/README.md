# id_effect_logger

`EffectLogger` capability for [`id_effect`](https://github.com/Industrial/id_effect): logging as
effectful steps, forwarded to [`tracing`](https://docs.rs/tracing) by default.

- `EffectLogger` methods (`trace` / `debug` / `info` / `warn` / `error` / `fatal` / `log`) each return an
  `id_effect::next::Effect<(), EffectLoggerError>` with no environment, so log lines compose with
  the rest of a program: `~logger.info("…")` binds inside any `effect_next!` (no turbofish), and
  `.narrow()` lifts a log step into a combinator chain over another environment.
- Provide it with `.provide(EffectLogger)` (or a `Layer`) and extract it inside `effect_next!`
  with `~EffectLogger`, or use it directly as a value.
- The thread-local pipeline (minimum level, annotations, spans, composite backend) is installed
  by building the providers (`provide_minimum_log_level`, `LogMetadataLive`,
  `provide_composite_logger`) with `build_env` on the thread that runs the program.
- Fiber-scoped minimum level (`with_minimum_log_level`), annotations (`annotate_logs`) and log spans
  (`with_log_span`).
- Pluggable backends: `TracingLogBackend`, `JsonLogBackend`, `StructuredLogBackend` and
  `CompositeLogBackend` (see `provide_composite_logger`).
- Messages accept `impl Into<Cow<'static, str>>`: literals stay zero-copy.

## Quick start

```rust
use id_effect::Exit;
use id_effect::next::{Effect, caps_next, effect_next};
use id_effect_logger::{EffectLogger, EffectLoggerError};

// As a plain value:
assert_eq!(EffectLogger.info("service starting").block_on(), Exit::Success(()));

// As a capability inside `effect_next!`:
let prog: Effect<u32, EffectLoggerError, caps_next!(EffectLogger)> = effect_next!({
  let logger = ~EffectLogger;
  ~logger.info("handling request");
  42
});
assert_eq!(prog.provide(EffectLogger).block_on(), Exit::Success(42));
```

Install a `tracing` subscriber (e.g. `tracing_subscriber::fmt().init()`) to see the output.

## Examples

| Example | Shows |
|---------|-------|
| `effect_logger` | every level on `EffectLogger` as a plain value |
| `context_service` / `layer_build` | providing `EffectLogger` (value / `Layer`) and extracting it with `~EffectLogger` |
| `log_effects` | all five levels as `~logger.level(…)` steps |
| `macro_sync_bind` / `macro_block_tail` | mixing log steps with other binds in `effect!` |

Run one with `cargo run -p id_effect_logger --example log_effects`.
