# p-config-unify: `Config<T>` is the only `id_effect_config` API

0.7 makes the `Config<T>` descriptor the single way to read configuration (Effect.ts `Config`).
The free loaders and free combinators are gone, `Config::all` accumulates errors, a descriptor
can be turned into a layer, and every provider installs itself as the capability.

## Removed → replacement

| 0.6 / m-config | 0.7 |
|---|---|
| `config::string(&p, "K")`, `config::integer`, `number`, `boolean`, `optional_string`, `string_list` | `Config::string("K").load(&p)` (`Config::integer`, …) |
| `config::nested_string(&p, "NS", "K")` (every `nested_*`) | `Config::string("K").nested("NS").load(&p)` or `Config::string("NS.K")` |
| `config::with_default(r, d)` | `Config::…(..).with_default(d)` |
| `config::all(a, b)`, `all3(a, b, c)`, `all_vec(v)` | `Config::all((a, b))`, `Config::all((a, b, c))` (tuples up to 12), `Config::all(vec)`, `Config::all([..; N])` |
| `zip_with(a, b, f)` | `a.zip_with(b, f)` (and `a.zip(b)`) |
| `or_else_config(a, b)` / `config::or_else(a, b)` | `a.or_else(b)` |
| `nest("NS", c)` | `c.nested("NS")` |
| `read_string(&["K"])`, `read_i64`, `read_bool`, `read_number`, `read_string_opt`, `read_string_list`, `read_nested_*`, `nested_path` | `Config::string("K").run()` (`Config::integer(..).run()`, …; nesting with `.nested(..)`) |
| `WithConfigDefault::with_default` on an effect | `Config::with_default` on the descriptor, before `.run()` |
| the `id_effect_config::config` module | import `Config` from the crate root |

## Behaviour changes

- **`Config::all` accumulates** (Effect.ts `Config.all`): every component is loaded; two or more
  failures come back as one `ConfigError::And(Vec<ConfigError>)` (flat, in component order); one
  failure is returned as is. `ConfigError::errors()` lists the individual failures.
  0.6 `all` stopped at the first failure. `ConfigError` is `#[non_exhaustive]`; a `match` that
  rendered variants needs an arm (or `_`) for `And`.
- `with_default` and `or_else` fall back when **every** failure is a missing key
  (`ConfigError::is_missing_only`, Effect `isMissingDataOnly`), so they also apply to a composite
  `Config::all` whose keys are all absent. An invalid key still propagates.

## New

- `Config::<T>::layer::<E>() -> Layer<HCons<T, HNil>, E, ConfigEnv>`: loads the descriptor when
  the layer is built and provides the value as a capability (Effect `Layer.effect(Tag, config)`).
  `E: From<ConfigError>`; a load failure fails the build.
- `ConfigProvider::layer::<E>()` on every provider (Effect `Layer.setConfigProvider`), same as
  `provide_config_provider(p)`. `ConfigProviderService::new(p)`; the service itself implements
  `ConfigProvider`.

```rust
// 0.6
let p = EnvConfigProvider::from_env();
let host = config::nested_string(&p, "SERVER", "HOST")?;
let port = config::with_default(config::nested_integer(&p, "SERVER", "PORT"), 8080)?;

// 0.7 — synchronous
let (host, port) = Config::all((Config::string("HOST"), Config::integer("PORT").with_default(8080)))
  .nested("SERVER")
  .load(&EnvConfigProvider::from_env())?;

// 0.7 — the loaded value as a capability
struct Server { host: String, port: i64 }
let server = Config::all((Config::string("HOST"), Config::integer("PORT").with_default(8080)))
  .nested("SERVER")
  .map(|(host, port)| Server { host, port })
  .layer::<AppError>();
let app = EnvConfigProvider::from_env().layer().provide_to(server);
program.provide_layer(app) // program: Effect<_, AppError, HCons<Server, HNil>>
```

Callers adapted in this leaf: `id_effect_axum` (`HostConfig::descriptor` uses `Config::all`) and
`id_effect_opentelemetry` (`load_otel_config` uses `Config::string(..).load`). `cli-minimal` and
labs `id_effect_ai` already used only `Config<T>` / `Secret`.
