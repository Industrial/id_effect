# Config + `Secret` from flags

[`id_effect_config`](../part2/ch07-10-config.md) already documents providers and [`Config`](https://docs.rs/id_effect_config/latest/id_effect_config/struct.Config.html) descriptors. At a **CLI edge**, you usually:

1. **Parse** a `--token` / `--api-key` flag (or read a path to a file).
2. **Seed** a [`MapConfigProvider`](https://docs.rs/id_effect_config/latest/id_effect_config/struct.MapConfigProvider.html) (or `EnvConfigProvider`) so the key matches what your `Config::*` descriptors expect.
3. **Evaluate** `Config::…` inside an `Effect` whose `R` is [`ConfigEnv`](https://docs.rs/id_effect_config/latest/id_effect_config/type.ConfigEnv.html) (`HCons<ConfigProviderService, HNil>`), built with [`config_env(provider)`](https://docs.rs/id_effect_config/latest/id_effect_config/fn.config_env.html).
4. **Wrap** sensitive strings with [`Secret`](https://docs.rs/id_effect_config/latest/id_effect_config/struct.Secret.html) via [`Config::secret`](https://docs.rs/id_effect_config/latest/id_effect_config/struct.Config.html#method.secret) so logs and `Debug` never print raw material.

## Minimal snippet

From `examples/cli-minimal/src/lib.rs`:

```rust
use id_effect::{Cause, Effect};
use id_effect_config::{Config, ConfigEnv, ConfigError, MapConfigProvider, Secret, config_env};

/// The `API_TOKEN` secret, loaded from the `ConfigEnv` capability.
fn token_secret() -> Effect<Secret<String>, ConfigError, ConfigEnv> {
    Config::string("API_TOKEN")
        .secret()
        .run::<Secret<String>, ConfigError, ConfigEnv, _>()
}

/// The program and the matching environment, built from the flag.
pub fn app_effect(token: String) -> (Effect<(), ConfigError, ConfigEnv>, ConfigEnv) {
    let provider = MapConfigProvider::from_pairs([("API_TOKEN", token.as_str())]);
    let env = config_env(provider);
    (token_secret().map(|_secret| ()), env)
}

/// A synchronous loader for callers outside any effect.
pub fn load_token_secret(token: &str) -> Result<Secret<String>, Cause<ConfigError>> {
    let env = config_env(MapConfigProvider::from_pairs([("API_TOKEN", token)]));
    token_secret().provide_env(env).block_on().into_result()
}
# assert!(load_token_secret("t0ken").is_ok());
# let (eff, env) = app_effect("t0ken".to_owned());
# assert!(eff.provide_env(env).block_on().into_result().is_ok());
```

`main` then calls `run_main(eff, env, RunMainConfig::with_tracing())` ([CLI with clap](./ch16-00-cli-with-clap.md)). The repository ships the full runnable layout under **[`examples/cli-minimal`](https://github.com/Industrial/id_effect/tree/main/examples/cli-minimal)** (`cli_minimal` package in the workspace).

## Operational notes

- Prefer **short-lived** exposure of secrets: parse → wrap in `Secret` → pass to effects; read the raw value only with `Secret::expose`, and avoid storing raw `String` copies in globals.
- For file-based secrets, read bytes in an effect and wrap immediately; redact paths in error messages if they reveal usernames.
- Combine with [CLI with clap](./ch16-00-cli-with-clap.md) for argv parsing and [exit codes](./ch16-01-cli-exit-codes.md) for `main`.
