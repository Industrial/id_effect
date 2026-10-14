# Logging (`id_effect_logger`)

Workspace crate **`id_effect_logger`** provides an injectable **`EffectLogger`** service: log lines are **effect steps** that read the logger capability from **`R`**, so formatting and sinks stay composable and testable.

## Usage shape

- `*~EffectLogger` inside **`effect!`** (with `|r|`) to obtain the handle, then **`info`**, **`warn`**, … on static or dynamic messages (`impl Into<Cow<'static, str>>`).
- Each log method returns **`Effect<(), EffectLoggerError>`** — no environment. `~log.info("…")` binds inside any `effect!` environment, so there is **no `::<R>` turbofish** (0.5 needed `log.info::<caps!(…)>(…)`). Outside `effect!`, lift a log step into another environment with **`.into_env()`**.
- **`EffectLoggerLive`** (plus `provide_composite_logger`, `provide_minimum_log_level`, `LogMetadataLive`) are providers you pass to **`run_with`** / **`build_env`**.

```rust
use id_effect::{Effect, Env, effect, caps, provide, run_with, succeed};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

fn app() -> Effect<(), EffectLoggerError, caps!(EffectLogger)> {
    effect!(|r| {
        let log = *~EffectLogger;
        ~log.info("hello");
    })
}

run_with([provide!(EffectLoggerLive)], app())?;

// In a combinator chain over `Env`:
let step: Effect<u32, EffectLoggerError, Env> =
    succeed(1).flat_map(|n| EffectLogger.info("got n").into_env().map(move |()| n));
```

## Backends

The crate ships pipeline pieces such as **structured JSON**, **tracing** integration, and **composite** backends—see `cargo doc -p id_effect_logger` for **`LogBackend`**, **`JsonLogBackend`**, **`TracingLogBackend`**, **`CompositeLogBackend`**.

## Relation to the rest of Part II

Logging is just another **capability**: same mental model as [Capability services](./ch05-00-tags-context.md) and [Providing Services](./ch07-03-providing-services.md). Swap backends in tests via `provide!(…)` instead of silencing `println!`.

## Further reading

- `cargo doc --open -p id_effect_logger`
