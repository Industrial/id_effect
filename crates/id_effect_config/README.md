# id_effect_config

Configuration for [`id_effect`](https://github.com/Industrial/id_effect) programs, in three
complementary styles:

1. **`Config<T>` descriptors** (recommended) — compose lazy descriptors such as
   `Config::string("HOST")` and `Config::integer("PORT").with_default(3000)`, then evaluate them
   directly with `Config::load(&provider)` or as an `Effect` with `Config::run` against an
   environment that holds a `ConfigProvider`.
2. **Figment + serde** — layer TOML / JSON / env sources into a `Figment` and `extract` a whole
   struct.
3. **Low-level reads** — `read_string`, `read_i64`, … as effects whose environment holds the
   `ConfigProviderService` capability.

The effects are `id_effect::next` effects (typed environment `R`); the provider capability comes
from `config_env(provider)` or from a layer (`provide_config_provider`,
`EnvConfigProviderLive::layer()`, `provide_figment_config_provider`, …).

Providers: `EnvConfigProvider` (process environment), `MapConfigProvider` (in-memory pairs, ideal
for tests), `FigmentConfigProvider` and `OrElseConfigProvider` (fallback chains). `Secret<T>` keeps
sensitive values out of `Debug` output. Cargo features `toml` / `env` (default), `json`, `yaml` and
`dotenv` select Figment sources.

## Quick start

```rust
use id_effect::Exit;
use id_effect_config::{Config, ConfigError, MapConfigProvider, config, config_env};

let provider = MapConfigProvider::from_pairs([("HOST", "localhost"), ("PORT", "8080")]);

// Descriptors — nothing is read yet.
let host = Config::string("HOST");
let port = Config::integer("PORT").with_default(3000);

// Evaluate synchronously against a provider…
let (h, p) = config::all(host.clone(), port.clone()).load(&provider).unwrap();
assert_eq!((h.as_str(), p), ("localhost", 8080));

// …or as an Effect with the provider injected through the environment.
let env = config_env(provider);
let h2 = host.run::<String, ConfigError, _, _>().provide_all(env).block_on();
assert!(matches!(h2, Exit::Success(h) if h == "localhost"));
```

## Examples

| Example | Shows |
|---------|-------|
| `001_extract_toml` | deserialize a struct from a TOML file with Figment |
| `002_map_provider` | `MapConfigProvider` with nested keys and defaults |
| `003_figment_provider_layer` | a Figment-backed provider layer |
| `004_or_else_provider` | falling back from one provider to another |

Run one with `cargo run -p id_effect_config --example 002_map_provider`.
