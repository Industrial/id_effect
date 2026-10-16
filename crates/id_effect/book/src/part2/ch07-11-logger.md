# Logging (`id_effect_logger`)

Workspace crate **`id_effect_logger`** provides an injectable **`EffectLogger`** service: log lines
are **effect steps**, so formatting and sinks stay composable and testable.

## Usage shape

- `EffectLogger` is a unit capability. Read it with `~EffectLogger` inside `effect!` (a
  `&Arc<EffectLogger>`), then call **`info`**, **`warn`**, … with static or dynamic messages
  (`impl Into<Cow<'static, str>>`).
- Each log method returns **`Effect<(), EffectLoggerError>`** (`R = HNil`), so `~logger.info("…")`
  binds inside any `effect!`. Outside the macro, lift a log step into a larger environment with
  **`.narrow()`**; at the edge it runs on its own: `EffectLogger.warn("plain").block_on()`.
- **`EffectLoggerLive::layer()`**, **`LogMetadataLive::layer()`**, **`provide_composite_logger(c)`**
  and **`provide_minimum_log_level(level)`** are `Layer`s (no input, any `E`).

<!-- ignore: needs the `id_effect_logger` crate, which the book_snippets harness does not depend on -->
```rust,ignore
use id_effect::{Effect, Exit, caps, effect};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

fn app() -> Effect<u32, EffectLoggerError, caps!(EffectLogger)> {
    effect!({
        let logger = ~EffectLogger;
        ~logger.info("all good");
        7
    })
}

assert_eq!(app().provide_layer(EffectLoggerLive::layer()).block_on(), Exit::Success(7));
// `EffectLogger` is a plain value too:
assert_eq!(app().provide(EffectLogger).block_on(), Exit::Success(7));
```

A full pipeline is a merged layer:

<!-- ignore: needs the `id_effect_logger` crate, which the book_snippets harness does not depend on -->
```rust,ignore
let pipeline = LogMetadataLive::layer()
    .merge(provide_composite_logger(composite))
    .merge(provide_minimum_log_level(LogLevel::Info));
let exit = program.provide_layer(pipeline).block_on();
```

## Levels, annotations, spans

- The minimum level is a `FiberRef<LogLevel>` (`EffectLogMinLevel`): set it inside the program, or
  scope it with `EffectLogger::with_minimum_log_level(fr, level, inner)`. Each run starts from the
  level given to `provide_minimum_log_level`.
- `annotate_logs` and `with_log_span` wrap an effect; forked children inherit a copy of the
  parent's annotations and spans.
- The backend pipeline is installed **per thread** when the layers are built. A program run with
  `block_on` sees what its thread installed; fibers on Tokio worker threads see only what those
  threads installed and otherwise fall back to `tracing`.

## Backends

`LogBackend` implementations include **`TracingLogBackend`**, **`JsonLogBackend`**,
**`StructuredLogBackend`** and **`CompositeLogBackend`**; `id_effect_opentelemetry` adds
`OtelLogBackend`. See `cargo doc -p id_effect_logger`.

## Relation to the rest of Part II

Logging is just another **capability**: same mental model as [Providing Services](./ch07-03-providing-services.md).
Swap backends in tests with a different layer instead of silencing `println!`.

## Further reading

- Examples: `crates/id_effect_logger/examples` (`layer_build`, `effect_logger`, `log_effects`, …)
- `cargo doc --open -p id_effect_logger`
