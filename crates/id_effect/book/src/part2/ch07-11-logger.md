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

```rust
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

```rust
# use std::sync::Arc;
# use id_effect::{caps, succeed};
# use id_effect_logger::{
#   CompositeLogBackend, EffectLogComposite, EffectLoggerError, LogLevel, LogMetadataLive,
#   provide_composite_logger, provide_minimum_log_level,
# };
# let composite = Arc::new(CompositeLogBackend::new());
# let program = succeed::<(), EffectLoggerError, caps!(EffectLogComposite)>(());
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

A `LogRecord` is `#[non_exhaustive]` (since 0.10): build one with `LogRecord::new(level, msg)`
and `with_target` / `with_annotation` / `with_spans`. `target` is the line's module or category;
`EffectLogger::log_target(target, level, msg)` sets it.

## Redaction and sampling

A **`RedactionPolicy`** names what must never reach a sink: annotation keys (`field`, ASCII
case-insensitive), key patterns (`glob`, `*` and `?`), and known secret values (`secret`, or
`redacted` for a `Redacted<T>`) masked wherever they occur — message, target, annotation values,
spans. Register a config `Secret<T>` with `policy.secret(key.expose())` at startup.
`provide_redaction_policy(policy)` installs it; the logger applies it **once per line, before the
composite fans out**, so tracing, JSON, structured and OTel backends all see the same masked
record. With no policy installed the logger applies `RedactionPolicy::recommended()` (passwords,
tokens, secrets, cookies, `authorization`, `session`, `csrf`, …); an installed policy replaces it,
so extend `recommended()` or start from `RedactionPolicy::empty()`. `RedactionPolicy::dangerous_none()`
turns redaction off and records the opt-out for the prod posture check. The tracing and structured
line formats escape control characters (CR, LF, ANSI escapes) and the spaces and `=` in keys, so a
logged value cannot forge a line or a field.

**`SamplingBackend`** wraps any backend for overload protection: `ratio(level, r)` keeps a
deterministic fraction of lines at that level, `rate_limit(burst, per_second)` is a token bucket
over all levels, and `exempt_at_or_above(level)` lets severe lines through. `stats()` reports
`emitted`, `sampled_out` and `rate_limited`.

```rust
use std::sync::Arc;

use id_effect::{Effect, Exit, HNil};
use id_effect_logger::{
    CompositeLogBackend, EffectLogger, EffectLoggerError, JsonLogBackend, LogLevel,
    LogMetadataLive, Logger, RedactionPolicy, SamplingBackend, annotate_logs,
    provide_composite_logger, provide_redaction_policy,
};

let json = JsonLogBackend::new(Vec::<u8>::new());
let sampled = Arc::new(
    SamplingBackend::new(Arc::new(json.clone()))
        .ratio(LogLevel::Debug, 0.1)
        .rate_limit(1_000, 200.0)
        .exempt_at_or_above(LogLevel::Error),
);
let composite = Arc::new(CompositeLogBackend::new());
composite.add(sampled.clone()).unwrap();

let policy = RedactionPolicy::recommended().glob("*token*");
let pipeline = LogMetadataLive::layer()
    .merge(provide_composite_logger(composite))
    .merge(provide_redaction_policy(policy));

let program: Effect<(), EffectLoggerError, HNil> =
    annotate_logs("password", "hunter2", EffectLogger.info("signed in"));
assert_eq!(program.provide_layer(pipeline).block_on(), Exit::Success(()));

let line = String::from_utf8(json.writer_arc().lock().unwrap().clone()).unwrap();
assert!(line.contains(r#""password":"<redacted>""#));
assert_eq!(sampled.stats().emitted, 1);
```

Redaction is a pipeline step, not a backend: a `LogBackend` called directly (outside
`EffectLogger`) is not filtered. Sampling, by contrast, is per backend — wrap only the sinks that
need protecting (a remote exporter, say) and leave a local file unsampled.

## Relation to the rest of Part II

Logging is just another **capability**: same mental model as [Providing Services](./ch07-03-providing-services.md).
Swap backends in tests with a different layer instead of silencing `println!`.

## Further reading

- Examples: `crates/id_effect_logger/examples` (`layer_build`, `effect_logger`, `log_effects`, …)
- `cargo doc --open -p id_effect_logger`
