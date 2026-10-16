//! `VaultSettings`: the database password comes from HashiCorp Vault (KV v2), merged above the
//! profile files and below the environment.
//!
//! Start a dev server and store the secret:
//!
//! ```text
//! docker run --rm -p 8200:8200 -e VAULT_DEV_ROOT_TOKEN_ID=root hashicorp/vault:1.20
//! VAULT_ADDR=http://127.0.0.1:8200 VAULT_TOKEN=root vault kv put secret/ledger/db password=s3cr3t
//! ```
//!
//! Run: `VAULT_TOKEN=root cargo run -p id_effect_config --features vault --example 050_vault --
//! --profile prod` (`VAULT_ADDR` defaults to `http://127.0.0.1:8200`).

use std::time::Duration;

use id_effect::{Cause, Effect, Exit, HCons, HNil, need};
use id_effect_config::{Config, ConfigError, Profiles, Secret, VaultAuth, VaultSettings};
use id_effect_tokio::TokioRuntime;

/// Database settings: host from the profile files, password from Vault.
struct DbConfig {
  host: String,
  password: Secret<String>,
}

fn db_config() -> Config<DbConfig> {
  Config::all((
    Config::string("db.host"),
    Config::string("db.password").secret(),
  ))
  .map(|(host, password)| DbConfig { host, password })
}

fn describe() -> Effect<String, ConfigError, HCons<DbConfig, HNil>> {
  need::<DbConfig>().map(|db| {
    format!(
      "db host {} with password {:?} ({} characters)",
      db.host,
      db.password,
      db.password.expose().len()
    )
  })
}

#[tokio::main]
async fn main() {
  // A throwaway `config/` directory standing in for the one shipped with the application.
  let root = tempfile::tempdir().expect("tempdir");
  let dir = root.path().join("config");
  std::fs::create_dir(&dir).expect("config dir");
  std::fs::write(
    dir.join("base.toml"),
    "[db]\nhost = \"localhost\"\npassword = \"dev-only\"\n",
  )
  .expect("base");
  std::fs::write(dir.join("prod.toml"), "[db]\nhost = \"db.internal\"\n").expect("prod");

  let addr = std::env::var("VAULT_ADDR").unwrap_or_else(|_| "http://127.0.0.1:8200".into());
  let auth = match VaultAuth::token_from_env() {
    Ok(auth) => auth,
    Err(e) => {
      eprintln!("set VAULT_TOKEN: {e}");
      std::process::exit(2);
    }
  };
  let vault = VaultSettings::new(addr, auth)
    .path_at("ledger/db", "db")
    .profiles(Profiles::new().dir(&dir))
    .refresh_every(Duration::from_secs(60));

  // Vault + profiles layer (the configuration capability) → config layer (`DbConfig`).
  let app = vault.layer::<ConfigError>().provide_to(db_config().layer());
  let runtime = TokioRuntime::current().expect("inside tokio::main");
  match runtime.run(describe().provide_layer(app)).await {
    Exit::Success(line) => println!("{line}"),
    Exit::Failure(Cause::Fail(e)) => {
      eprintln!("configuration failed: {e}");
      std::process::exit(1);
    }
    Exit::Failure(cause) => panic!("configuration failed: {cause:?}"),
  }
}
