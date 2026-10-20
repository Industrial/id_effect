//! `ledger`: load the profile, open what it names, serve until SIGINT / SIGTERM.
//!
//! ```text
//! cargo run -p ledger                      # profile dev (compose.yaml)
//! cargo run -p ledger -- --profile prod    # secrets from Vault (VAULT_TOKEN)
//! ```
//!
//! Before serving, it prints who may call each route and the security posture report (ADR 0013
//! R10); under any profile but `dev` and `test` a failed probe stops it. The Ledger accepts no
//! risk: no profile has a `[security.accepted_risks]` table.

use std::process::ExitCode;
use std::time::Duration;

use id_effect::{Cause, Effect, Exit, Never};
use id_effect_kit::axum::{EffectExecutor, serve_until_shutdown};
use id_effect_kit::logger::provide_redaction_policy;
use id_effect_kit::posture::Posture;
use ledger::app::host_config;
use ledger::config::{self, LedgerConfig};
use ledger::{StartupError, assemble, background, limits, observability, posture, start};

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
    config::load(&profiles, profile.clone(), None).map_err(StartupError::from);
  let config = match EffectExecutor::global().run_effect(loaded).await {
    Exit::Success(config) => config,
    Exit::Failure(cause) => return fail(describe(cause)),
  };
  // Traces, metrics and logs over OTLP when the environment names a collector.
  let otel = match observability::install() {
    Ok(otel) => otel,
    Err(e) => return fail(format!("opentelemetry: {e}")),
  };
  let redaction = provide_redaction_policy::<Never>(observability::redaction(&config)).build();
  let _ = EffectExecutor::global().run_effect(redaction).await;
  let mut services = match EffectExecutor::global().run_effect(start(&config)).await {
    Exit::Success(services) => services,
    Exit::Failure(cause) => return fail(describe(cause)),
  };
  services.env_filter = otel.as_ref().map(|guard| guard.env_filter().clone());
  if config.auth.reload_limits_secs > 0 {
    let every = Duration::from_secs(config.auth.reload_limits_secs);
    match limits::watch(&profiles, profile, every) {
      Ok(watch) => services.limits = Some(watch),
      Err(e) => return fail(format!("login limits: {e}")),
    }
  }
  let tasks = match EffectExecutor::global()
    .run_effect(background::tasks(&config, &services))
    .await
  {
    Exit::Success(tasks) => tasks,
    Exit::Failure(cause) => return fail(describe(cause)),
  };
  let app = match assemble(&config, services) {
    Ok(app) => app,
    Err(e) => return fail(e),
  };
  eprint!("{}", app.access_report());
  // Once everything is assembled, so every opt-out it took is in the posture registry.
  let base = match Posture::load(&profiles) {
    Ok(base) => base,
    Err(e) => return fail(e),
  };
  match posture(&config, &app, base).check().enforce() {
    Ok(report) => eprint!("{report}"),
    Err(refused) => return fail(refused),
  }
  let running: Vec<_> = tasks
    .into_iter()
    .map(|(name, task)| {
      eprintln!("ledger: {name} started");
      tokio::spawn(EffectExecutor::global().run_effect(task))
    })
    .collect();
  let program = serve_until_shutdown(app.router).provide(host_config(&config));
  let served = EffectExecutor::global().run_effect(program).await;
  // Dropping a task's future interrupts its fiber: the worker drains, the cron resigns.
  for task in running {
    task.abort();
  }
  if let Some(otel) = otel {
    otel.shutdown();
  }
  match served {
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
