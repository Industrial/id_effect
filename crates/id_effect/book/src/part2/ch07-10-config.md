# Configuration (`id_effect_config`)

Workspace crate **`id_effect_config`** loads configuration the Effect.ts way: lazy **`Config<T>`
descriptors** evaluated against a **`ConfigProvider`** (environment, maps, Figment, fallbacks,
scoped prefixes).

## Descriptors

```rust,no_run
# fn main() -> Result<(), id_effect_config::ConfigError> {
use id_effect_config::{Config, EnvConfigProvider};

let (host, port) = Config::all((
    Config::string("HOST"),
    Config::integer("PORT").with_default(8080),
))
.nested("SERVER") // SERVER_HOST, SERVER_PORT
.load(&EnvConfigProvider::from_env())?;
# let _: (String, i64) = (host, port);
# Ok(())
# }
```

- Primitives: `Config::string`, `integer`, `number`, `boolean`, … ; `Secret<T>` for values that must
  not be printed.
- Combinators: `with_default`, `or_else`, `nested`, `map`, `zip` / `zip_with`, and `Config::all`
  over tuples (up to 12), vectors and arrays.
- **`Config::all` accumulates**: every component is loaded and two or more failures come back as
  one `ConfigError::And(..)`. `with_default` / `or_else` apply only when every failure is a missing
  key.
- `load(&provider)` is synchronous; `run()` is the same read as an `Effect` over any `R` that
  contains `ConfigProviderService`.

## Providers as layers

Every provider is also a layer (`provider.layer()`, or `provide_config_provider(p)`) whose output
is `ConfigEnv = HCons<ConfigProviderService, HNil>`. A descriptor turns into a layer that provides
the **loaded value** as a capability, so application code needs the config, not the provider
(`examples/005_config_layer.rs`):

```rust
use id_effect::{Effect, Exit, HCons, HNil, need};
use id_effect_config::{Config, ConfigError, ConfigProvider, MapConfigProvider};

struct ServerConfig { host: String, port: i64 }

fn server_config() -> Config<ServerConfig> {
    Config::all((
        Config::string("HOST").with_default("127.0.0.1".into()),
        Config::integer("PORT"),
    ))
    .map(|(host, port)| ServerConfig { host, port })
    .nested("SERVER")
}

fn describe() -> Effect<String, ConfigError, HCons<ServerConfig, HNil>> {
    need::<ServerConfig>().map(|c| format!("listening on {}:{}", c.host, c.port))
}

let provider = MapConfigProvider::from_pairs([("SERVER_PORT", "8080")]);
let app = provider.layer().provide_to(server_config().layer());
let Exit::Success(line) = describe().provide_layer(app).block_on() else { panic!() };
assert_eq!(line, "listening on 127.0.0.1:8080");
```

`Config::<T>::layer::<E>()` needs `E: From<ConfigError>`; a load failure fails the layer build.
For a quick read with a ready provider:
`Config::integer("PORT").run::<i64, ConfigError, _, _>().provide_all(config_env(p)).block_on()`.

## Figment + serde

Build a **[Figment](https://docs.rs/figment)** (TOML + env + …) with the `figment` helpers, then
`extract` the whole document into a serde type, or wrap it as a `FigmentConfigProvider` to use it
with descriptors.

## Further reading

- Examples: `crates/id_effect_config/examples` (`002_map_provider` … `041_scoped_config_provider`)
- `cargo doc --open -p id_effect_config`
- [Schema](../part4/ch14-00-schema.md) for validating structured values *after* config strings become wire data
