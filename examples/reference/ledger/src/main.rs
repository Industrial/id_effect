//! `ledger`: load the profile, open what it names, serve until SIGINT / SIGTERM.
//!
//! ```text
//! cargo run -p ledger                      # profile dev (compose.yaml)
//! cargo run -p ledger -- --profile prod    # secrets from Vault (VAULT_TOKEN)
//! ```

use std::process::ExitCode;
use std::time::Duration;

use id_effect::{Cause, Effect, Exit};
use id_effect_kit::axum::{EffectExecutor, HostConfig, serve_until_shutdown};
use ledger::config::{self, LedgerConfig};
use ledger::{StartupError, router, start};

/// The directory of `base.toml` and the profiles, relative to the working directory.
const CONFIG_DIR: &str = "config";

#[tokio::main]
async fn main() -> ExitCode {
  let profiles = config::profiles(CONFIG_DIR);
  let profile = match profiles.resolve() {
    Ok(profile) => profile,
    Err(e) => return fail(e),
  };
  eprintln!("ledger: profile {profile}");
  let loaded: Effect<LedgerConfig, StartupError> =
    config::load(&profiles, profile, None).map_err(StartupError::from);
  let config = match EffectExecutor::global().run_effect(loaded).await {
    Exit::Success(config) => config,
    Exit::Failure(cause) => return fail(describe(cause)),
  };
  let services = match EffectExecutor::global().run_effect(start(&config)).await {
    Exit::Success(services) => services,
    Exit::Failure(cause) => return fail(describe(cause)),
  };
  let app = match router(&config, services) {
    Ok(app) => app,
    Err(e) => return fail(e),
  };
  let host = HostConfig {
    bind_host: config.http.host.clone(),
    bind_port: config.http.port,
    shutdown_timeout: Duration::from_secs(20),
  };
  let program = serve_until_shutdown(app).provide(host);
  match EffectExecutor::global().run_effect(program).await {
    Exit::Success(reason) => {
      eprintln!("ledger: stopped ({reason:?})");
      ExitCode::SUCCESS
    }
    Exit::Failure(cause) => fail(format!("{cause:?}")),
  }
}

fn describe(cause: Cause<StartupError>) -> String {
  match cause {
    Cause::Fail(e) => e.to_string(),
    other => format!("{other:?}"),
  }
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
  eprintln!("ledger: {message}");
  ExitCode::FAILURE
}
