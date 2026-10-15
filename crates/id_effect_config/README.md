# id_effect_config

Configuration for [`id_effect`](https://github.com/Industrial/id_effect) programs, modelled on the
Effect.ts `Config` module. There is one reading API, the lazy **`Config<T>` descriptor**:

- compose descriptors such as `Config::string("HOST")`, `Config::integer("PORT").with_default(3000)`,
  `.nested("SERVER")`, `.map(..)`, `.secret()`, `.or_else(..)`;
- combine them with `Config::all((a, b, c))` (tuples, `Vec`s and arrays), `a.zip(b)`,
  `a.zip_with(b, f)`. Like Effect.ts `Config.all`, **errors accumulate**: every missing or
  invalid key is reported in one `ConfigError::And`, not only the first;
- evaluate with `Config::run` (an `id_effect` effect reading the `ConfigProviderService`
  capability from its environment), `Config::layer()` (a `Layer` that provides the loaded value
  as a capability) or `Config::load(&provider)` (synchronous, for tests and edges).

**Providers are capabilities.** `EnvConfigProvider` (process environment), `MapConfigProvider`
(in-memory pairs, ideal for tests), `FigmentConfigProvider`, `OrElseConfigProvider` (fallback
chains) and `ScopedConfigProvider` (`provider.within("NS")`) are installed as the
`ConfigProviderService` capability with `provider.layer()` (Effect `Layer.setConfigProvider`),
`provide_config_provider(p)`, `EnvConfigProviderLive::layer()`, or `config_env(p)` for a ready
environment value.

Figment + serde remain available for whole-document deserialization (`extract`, `FigmentLayer`,
the `figment::*` builders). `Secret<T>` keeps sensitive values out of `Debug` output. Cargo
features `toml` / `env` (default), `json`, `yaml` and `dotenv` select Figment sources.

## Quick start

```rust
use id_effect::Exit;
use id_effect::{Effect, HCons, HNil, need};
use id_effect_config::{Config, ConfigError, ConfigProvider, MapConfigProvider};

let provider = MapConfigProvider::from_pairs([("HOST", "localhost"), ("PORT", "8080")]);

// Descriptors — nothing is read yet.
let server = Config::all((Config::string("HOST"), Config::integer("PORT").with_default(3000)));

// Evaluate synchronously against a provider…
let (h, p) = server.load(&provider).unwrap();
assert_eq!((h.as_str(), p), ("localhost", 8080));

// …or provide the loaded value as a capability through layers.
struct Server(String, i64);
let app = provider
  .layer()
  .provide_to(server.map(|(h, p)| Server(h, p)).layer::<ConfigError>());
let program: Effect<String, ConfigError, HCons<Server, HNil>> =
  need::<Server>().map(|s| format!("{}:{}", s.0, s.1));
assert!(matches!(program.provide_layer(app).block_on(), Exit::Success(a) if a == "localhost:8080"));

// Every failure is reported.
let empty = MapConfigProvider::from_pairs::<[(&str, &str); 0], _, _>([]);
let err = Config::all((Config::string("HOST"), Config::integer("PORT"))).load(&empty).unwrap_err();
assert_eq!(err.errors().len(), 2);
```

## Examples

| Example | Shows |
|---------|-------|
| `001_extract_toml` | deserialize a struct from a TOML file with Figment |
| `002_map_provider` | `MapConfigProvider` with nested keys, defaults and accumulated errors |
| `003_figment_provider_layer` | a Figment-backed provider layer |
| `004_or_else_provider` | falling back from one provider to another |
| `005_config_layer` | `Config::layer()`: the loaded config as a capability |
| `041_scoped_config_provider` | a request-local provider layer |

Run one with `cargo run -p id_effect_config --example 002_map_provider`.
