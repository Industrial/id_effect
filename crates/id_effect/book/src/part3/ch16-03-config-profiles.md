# Profiles: `dev`, `test`, `prod`

One binary, three environments. [`Profiles`](https://docs.rs/id_effect_config/latest/id_effect_config/profile/struct.Profiles.html) (in `id_effect_config`, default features `toml` + `env`) picks a profile at startup and layers the configuration for it, so the same [`Config`](../part2/ch07-10-config.md) descriptors read different values in development, CI and production.

## Layout

```text
config/
  base.toml   # shared by every profile
  dev.toml    # local development
  test.toml   # CI / integration tests
  prod.toml   # production
```

```toml
# config/base.toml
name = "ledger"
port = 3000

[db]
host = "localhost"
password = "dev-only"
```

```toml
# config/prod.toml
port = 8080

[db]
host = "db.internal"
```

## Precedence

Sources merge key by key; a later row overrides an earlier one.

| Rank | Source | Example |
|------|--------|---------|
| 1 (lowest) | `config/base.toml` | `port = 3000` |
| 2 | `config/<profile>.toml` | `port = 8080` |
| 3 | overlays added with `Profiles::overlay` (secret-manager snapshots) | `db.password` |
| 4 (highest) | `APP_`-prefixed environment variables | `APP_DB__PASSWORD=…` |

In short: **env > profile > base**. Environment keys lose the prefix, are lower-cased, and `__` nests: `APP_DB__PASSWORD` sets `db.password`. A missing TOML file is skipped (so `dev.toml` can be absent); a file that does not parse fails the load with `ConfigError::Figment`.

## Selecting the profile

Highest priority first:

1. `--profile prod` or `--profile=prod` on the command line (the last one wins);
2. the `ID_EFFECT_PROFILE` environment variable (empty counts as unset);
3. the default, `dev`.

Only declared profiles are accepted (`dev`, `test`, `prod` unless you call `.known([...])`). Anything else, a typo such as `ID_EFFECT_PROFILE=prdo` included, is a typed `ConfigError::UnknownProfile { profile, known }` that fails startup. A typo never silently falls back to the base configuration:

```text
unknown configuration profile "prdo" (known: dev, test, prod)
```

## Wiring it into a program

`Profiles::layer` resolves and loads the profile when the layer is built and installs the merged sources as the `ConfigProviderService` capability, so existing `Config::layer` / `Config::run` descriptors work unchanged:

```rust,no_run
use id_effect::{Effect, Exit, HCons, HNil, need};
use id_effect_config::{Config, ConfigError, Profiles, Secret};

struct Port(i64);
struct DbPassword(Secret<String>);

fn main() {
    // `config/` relative to the working directory, `ID_EFFECT_PROFILE` / `--profile`, `APP_` env.
    let config = Profiles::new().layer::<ConfigError>();

    let port = Config::integer("port").map(Port).layer::<ConfigError>();
    let program: Effect<i64, ConfigError, HCons<Port, HNil>> = need::<Port>().map(|p| p.0);

    match program.provide_layer(config.provide_to(port)).block_on() {
        Exit::Success(port) => println!("listening on {port}"),
        Exit::Failure(cause) => eprintln!("bad configuration: {cause:?}"),
    }

    // Secrets keep their redaction whatever layer they come from.
    let _password = Config::string("db.password").secret().map(DbPassword);
}
```

Paths are the dotted, lower-case TOML keys: `Config::string("db.host")`.

Outside an effect, `Profiles::new().load()?` returns a `ProfiledConfig` with the chosen `profile()`, the merged `figment()`, `extract::<T>()` for a whole serde struct, and `provider()` for descriptors.

## Customising

```rust
use id_effect_config::Profiles;

let profiles = Profiles::new()
    .dir("deploy/config")            // instead of ./config
    .known(["dev", "test", "staging", "prod"])
    .default_profile("dev")
    .env_var("APP_PROFILE")          // instead of ID_EFFECT_PROFILE
    .env_prefix("LEDGER_");          // LEDGER_DB__HOST instead of APP_DB__HOST
assert_eq!(profiles.known_profiles().len(), 4);
```

`Profiles::select(args, env_value)` is the pure form of the selection rules, handy in tests: `profiles.select(["--profile", "test"], None)`.

## Secrets per profile

Keep real secrets out of `prod.toml`. Either inject them through the environment (`APP_DB__PASSWORD`), or add a secret-manager snapshot with `Profiles::overlay(provider)`: overlays sit above the profile files and below the environment, so production reads the managed secret while a developer can still override it locally. Read them with `Config::…().secret()` so `Debug` and logs print `<redacted>` (see [Config + `Secret` from flags](./ch16-02-cli-config-secret.md)).
