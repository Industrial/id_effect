# Secrets from HashiCorp Vault

Keep production secrets out of `prod.toml` and out of the process environment: read them from [HashiCorp Vault](https://developer.hashicorp.com/vault/docs/secrets/kv/kv-v2). `id_effect_config` ships a Vault provider behind the non-default `vault` feature. It reads **KV version 2** secrets over HTTP (reqwest with rustls); there is no cloud SDK in the dependency tree.

```toml
[dependencies]
id_effect_config = { version = "0.11", features = ["vault"] }
id_effect_tokio = "0.11" # Vault requests run on a tokio runtime
```

## Read once, in a layer

`ConfigProvider::load_raw` is synchronous, and a network call is not. So the secrets are fetched **once, when the layer is built**, into an in-memory snapshot, and every `Config` lookup is then served from memory. The shorthand takes an address, an authentication method and the secret paths:

```rust,no_run
use id_effect::{Effect, Exit, HCons, HNil, need};
use id_effect_config::{Config, ConfigError, Secret, VaultAuth, VaultProvider};
use id_effect_tokio::TokioRuntime;

struct DbPassword(Secret<String>);

#[tokio::main]
async fn main() -> Result<(), ConfigError> {
    // GET http://127.0.0.1:8200/v1/secret/data/ledger/db with the X-Vault-Token header.
    let vault = VaultProvider::layer::<ConfigError, _, _>(
        "http://127.0.0.1:8200",
        VaultAuth::token_from_env()?, // VAULT_TOKEN
        ["ledger/db"],
    );
    let password = Config::string("password").secret().map(DbPassword).layer::<ConfigError>();

    let program: Effect<usize, ConfigError, HCons<DbPassword, HNil>> =
        need::<DbPassword>().map(|p| p.0.expose().len());
    let runtime = TokioRuntime::current().expect("inside tokio::main");
    match runtime.run(program.provide_layer(vault.provide_to(password))).await {
        Exit::Success(n) => println!("password has {n} characters"),
        Exit::Failure(cause) => eprintln!("startup failed: {cause:?}"),
    }
    Ok(())
}
```

The secret's `data` keys become configuration keys (`password` above). Read them with `Config::…().secret()` (or deserialize a field as `Secret<String>`): `Debug` and `Display` print `<redacted>`, as in [Config + `Secret` from flags](./ch16-02-cli-config-secret.md).

## `VaultSettings`: mount, namespace, paths

`VaultProvider::layer` is `VaultSettings::new(addr, auth).paths(paths).layer()`. Use `VaultSettings` directly for anything else:

```rust,no_run
use std::time::Duration;
use id_effect_config::{ConfigError, VaultAuth, VaultSettings};

# fn settings() -> Result<VaultSettings, ConfigError> {
let vault = VaultSettings::new("https://vault.internal:8200", VaultAuth::token_from_env()?)
    .mount("team-a/kv")              // KV v2 engine path (default: "secret")
    .namespace("admin/team-a")       // X-Vault-Namespace (Vault Enterprise / HCP)
    .path("ledger/app")              // keys merged at the root
    .path_at("ledger/db", "db")      // keys merged under `db`: db.password, db.user
    .timeout(Duration::from_secs(5));
# Ok(vault)
# }
```

Each path is one `GET <addr>/v1/<mount>/data/<path>`. Later paths override earlier ones key by key. Keys are used as stored, so keep them lower-case like the TOML files.

`VaultSettings::fetch()` is the same read as an effect returning the `VaultProvider` itself, for code that wants `provider.secret::<String>("db.password")` (a `Secret<T>`), `provider.refresh()`, or `Profiles::overlay(provider)`.

## With profiles: env > Vault > profile > base

Hand the settings your [`Profiles`](./ch16-03-config-profiles.md) and the secrets are merged through `Profiles::overlay`, between the profile files and the environment:

| Rank | Source | Example |
|------|--------|---------|
| 1 (lowest) | `config/base.toml` | `db.password = "dev-only"` |
| 2 | `config/<profile>.toml` | `db.host = "db.internal"` |
| 3 | Vault secrets | `db.password` from `kv/ledger/db` |
| 4 (highest) | `APP_`-prefixed environment variables | `APP_DB__PASSWORD` set in a developer's shell |

```rust,no_run
use id_effect_config::{ConfigError, Profiles, VaultAuth, VaultSettings};

# fn layer() -> Result<(), ConfigError> {
let config = VaultSettings::new("https://vault.internal:8200", VaultAuth::token_from_env()?)
    .path_at("ledger/db", "db")
    .profiles(Profiles::new())       // --profile / ID_EFFECT_PROFILE, config/, APP_
    .layer::<ConfigError>();         // installs the merged view as the config capability
# let _ = config;
# Ok(())
# }
```

Production reads the managed password; a developer can still override it with `APP_DB__PASSWORD` without touching Vault. The runnable version is `crates/id_effect_config/examples/050_vault.rs`.

## Rotation: `refresh_every`

```rust,no_run
# use std::time::Duration;
# use id_effect_config::{VaultAuth, VaultSettings};
let vault = VaultSettings::new("https://vault.internal:8200", VaultAuth::token("…"))
    .path_at("ledger/db", "db")
    .refresh_every(Duration::from_secs(300));
```

The layer then forks a daemon fiber that re-fetches every secret on that interval and swaps the snapshot atomically: a lookup sees the old snapshot or the new one, never a mix. A failed refresh keeps the previous snapshot serving and is reported by `VaultProvider::last_refresh_error()`; `generation()` counts the successful fetches. The fiber is interrupted when the layer's scope closes. With profiles, a refresh re-reads the profile files too.

Values already copied out of the provider (a `DbPassword` built by `Config::layer` at startup) do not change. Read through the `ConfigProviderService` capability (`Config::run`) where a rotated value must be picked up.

## Errors

Startup fails with a typed `ConfigError`; neither the token nor a secret value appears in it:

| Failure | Error |
|---------|-------|
| Bad address, no paths, zero refresh interval | `ConfigError::Invalid { path: "vault.addr" \| "vault.paths" \| "vault.refresh_every", .. }` |
| Connection refused, TLS, timeout | `ConfigError::Vault { kind: VaultErrorKind::Transport, .. }` |
| Wrong token / policy (`403`), no such secret (`404`) | `ConfigError::Vault { kind: VaultErrorKind::Status(403), reason: "permission denied", .. }` |
| Not a KV v2 response (a KV v1 mount, an HTML proxy page) | `ConfigError::Vault { kind: VaultErrorKind::Decode, .. }` |
| `VAULT_TOKEN` unset or empty | `ConfigError::Missing { path: "VAULT_TOKEN" }` |

```text
vault secret "ledger/db": HTTP 403: permission denied
```

## Testing

- **Unit tests** point `VaultSettings` at a mocked HTTP server (the crate's own tests use `wiremock`) and assert on values and `ConfigError::Vault` kinds.
- **Integration tests** start the real dev server with testcontainers (`hashicorp/vault`, `VAULT_DEV_ROOT_TOKEN_ID`), write a secret with `POST /v1/secret/data/<path>`, then read it through the layer. Such a test prints `SKIP` when Docker is unavailable, and fails instead when `ID_EFFECT_REQUIRE_DOCKER=1`, which is how CI runs it.

Only token authentication is built in for now. AWS and GCP secret managers are planned for 0.10.
