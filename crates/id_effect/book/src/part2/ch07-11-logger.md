# Logging (`id_effect_logger`)

Workspace crate **`id_effect_logger`** provides an injectable **`EffectLogger`** service: log lines are **effect steps** that read the logger capability from **`R`**, so formatting and sinks stay composable and testable.

## Usage shape

- `~EffectLoggerKey` inside **`effect!`** (with `|r|`) to obtain the handle, then **`info`**, **`warn`**, … on static or dynamic messages (`impl Into<Cow<'static, str>>`).
- **`provide_effect_logger`** (and related constructors) build a [`ProviderSpec`](../../src/capability/provider.rs) you pass to **`run_with`** / **`build_env`**.

```rust
use id_effect::{Effect, effect, caps, provide, run_with};

fn app() -> Effect<(), (), caps!(EffectLoggerKey)> {
    effect!(|r| {
        let log = *~EffectLoggerKey;
        ~ log.info("hello");
        ()
    })
}

run_with([provide!(TracingLoggerLive)], app())?;
```

## Backends

The crate ships pipeline pieces such as **structured JSON**, **tracing** integration, and **composite** backends—see `cargo doc -p id_effect_logger` for **`LogBackend`**, **`JsonLogBackend`**, **`TracingLogBackend`**, **`CompositeLogBackend`**.

## Relation to the rest of Part II

Logging is just another **capability**: same mental model as [Capability Keys](./ch05-00-tags-context.md) and [Providing Services](./ch07-03-providing-services.md). Swap backends in tests via `provide!(…)` instead of silencing `println!`.

## Further reading

- `cargo doc --open -p id_effect_logger`
