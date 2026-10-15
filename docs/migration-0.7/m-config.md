# `id_effect_config` on the `next` kernel

0.7 ports `id_effect_config` to the fiber kernel (`id_effect::next`). The descriptors
(`Config<T>`, `config::all`, …), the providers (`EnvConfigProvider`, `MapConfigProvider`,
`FigmentConfigProvider`, `OrElseConfigProvider`, `ScopedConfigProvider`), `Secret<T>`,
`ConfigError`, the Figment helpers and the synchronous `Config::load` / `config::*` functions are
unchanged. What changed is every API that returns an effect or wires the provider in:

| 0.6 | 0.7 |
|---|---|
| `Config::run::<A, E, R>()`, `R: Needs<ConfigProviderService>` | `Config::run::<A, E, R, I>()`, `R: Selector<ConfigProviderService, I>` (a `next::Effect`) |
| `read_string::<A, E, R>(..)` (and every `read_*`) | `read_string::<A, E, R, I>(..)`, same bound |
| `WithConfigDefault` on a v1 `Effect` | on a `next::Effect` (`A: Clone` no longer needed) |
| `ConfigEnv = id_effect::Env` | `ConfigEnv = HCons<ConfigProviderService, HNil>` |
| `provide_config_provider(p) -> ProviderBox` | `provide_config_provider(p) -> Layer<ConfigEnv, E>` |
| `provide_env_config_provider(opts)`, `provide_figment_config_provider(fig)` | the same, returning `Layer<ConfigEnv, E>` |
| `provide!(EnvConfigProviderLive)` (`ProviderSpec`) | `EnvConfigProviderLive::layer()` |
| `Env::scoped([provide_config_provider(..)])` | `.provide_layer(provide_config_provider(..))` on the inner effect |

The layers never fail and need nothing (`In = HNil`); their error type `E` is the one of the
graph they join (`Layer::merge` / `provide_merge` / `provide_to` need one `E`).

```rust
// 0.6
let env = build_env([provide_config_provider(MapConfigProvider::from_pairs([("PORT", "8080")]))])?;
let port: i64 = run_blocking(Config::integer("PORT").run::<i64, ConfigError, _>(), env)?;

// 0.7
use id_effect::Exit;
let program = Config::integer("PORT")
  .run::<i64, ConfigError, ConfigEnv, _>()
  .provide_layer(provide_config_provider(MapConfigProvider::from_pairs([("PORT", "8080")])));
let Exit::Success(port) = program.block_on() else { /* failure */ };

// or with a ready environment value
let exit = Config::integer("PORT")
  .run::<i64, ConfigError, _, _>()
  .provide_all(config_env(MapConfigProvider::from_pairs([("PORT", "8080")])))
  .block_on();
```

Inside a larger typed environment, `R` and `I` are inferred (`Config::string("HOST").run()` in a
function returning `Effect<String, MyError, R>` with `R: Selector<ConfigProviderService, I>`).

**Code still on the v1 kernel** bridges with `id_effect::next::compat` until it switches:

```rust
use id_effect::next::compat::IntoV1With;
use id_effect::next::{HCons, HNil};

fn load_port<R: Needs<ConfigProviderService> + 'static>() -> id_effect::Effect<i64, ConfigError, R> {
  Config::integer("PORT")
    .run::<i64, ConfigError, ConfigEnv, _>()
    .into_v1_with(|r: &mut R| -> ConfigEnv { HCons(Arc::new(r.need().clone()), HNil) })
}
```

A v1 `Env` that should hold the provider inserts it directly:
`env.insert::<Cap<ConfigProviderService>>(ConfigProviderService(Arc::new(provider)))`
(`id_effect_axum::bootstrap_env` and `provide_host_config_env` do this; their signatures are
unchanged).

The free sync loaders (`config::string`, …) and the descriptor/loader split are untouched here;
`p-config-unify` makes `Config<T>` the sole API.
