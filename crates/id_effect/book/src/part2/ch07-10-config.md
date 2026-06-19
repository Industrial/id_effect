# Configuration (`id_effect_config`)

Workspace crate **`id_effect_config`** loads configuration in a way aligned with **Effect.ts configuration**: lazy **descriptors**, optional **Figment** layering, and low-level **effectful reads** from a provider installed in **`R`**.

## Three complementary styles

1. **`Config<T>` descriptors** (recommended for parity with Effect `Config.*`)  
   Compose values like `Config::string("HOST")`, `Config::integer("PORT").with_default(3000)`, then evaluate with **`Config::load`** against a concrete provider or **`Config::run`** as an **`Effect`** with config capabilities in **`R`**.

2. **Figment + serde**  
   Build a **[Figment](https://docs.rs/figment)** (TOML + env + …), then **`extract`** / provider helpers for whole-document deserialization when you prefer serde-shaped config files.

3. **Low-level `read_*` helpers**  
   Declare `caps!(ConfigProviderKey)` (or the crate's config key) and call **`read_string`**, **`read_integer`**, … for imperative-style reads that still stay inside the effect environment.

## Wiring

At the stack root, install a **config provider** with `provide!(…)` and combine with [Providers](./ch06-00-layers.md) the same way as databases or loggers:

```rust
run_with([provide!(ConfigLive), provide!(DatabaseLive)], app())?;
```

## Further reading

- `cargo doc --open -p id_effect_config` — extensive crate-level examples
- [Schema](../part4/ch14-00-schema.md) for validating structured values *after* config strings become wire data
