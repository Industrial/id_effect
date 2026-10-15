//! Ex 153 — Cookbook: a `run_blocking` CLI with everything wired at the edge.
//!
//! - argv is parsed with `clap`, configuration comes from the process environment
//!   (`EnvConfigProvider`), and capabilities are inserted into one `Env` in `main`.
//! - The program itself is a plain `Effect` generic over `Needs<…>`, so tests run it with a
//!   `MapConfigProvider` and a recording console instead.
//! - `run_main` drives it at the edge (`block_on` on the `next` runtime, via the v1 → next bridge)
//!   and maps success → exit 0, a typed failure → exit 1.
//!
//! Run:
//! - `cargo run -p cli_minimal --example 153_cookbook_cli_edge -- --name Ada`
//! - `GREET_TIMES=9 cargo run -p cli_minimal --example 153_cookbook_cli_edge` (exit code 1)
//!
//! Test: `cargo test -p cli_minimal --example 153_cookbook_cli_edge`

use std::fmt;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use id_effect::next::HNil;
use id_effect::next::compat::IntoNext;
use id_effect::{Cap, Effect, Env, Needs};
use id_effect_cli::{RunMainConfig, run_main};
use id_effect_config::{
  Config, ConfigError, ConfigProvider, ConfigProviderService, EnvConfigProvider, config_env,
};

/// Capability: where the program writes its output.
pub trait Console: Send + Sync {
  /// Write one line.
  fn line(&self, text: String);
}

/// The service type registered in `Env`.
pub type ConsoleService = Arc<dyn Console>;

/// Production console.
struct Stdout;

impl Console for Stdout {
  fn line(&self, text: String) {
    println!("{text}");
  }
}

/// Everything the program can fail with.
#[derive(Debug)]
pub enum AppError {
  /// Missing or malformed configuration.
  Config(ConfigError),
  /// `GREET_TIMES` outside `1..=3`.
  TooManyGreetings(i64),
}

impl From<ConfigError> for AppError {
  fn from(e: ConfigError) -> Self {
    AppError::Config(e)
  }
}

impl fmt::Display for AppError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      AppError::Config(e) => write!(f, "configuration: {e}"),
      AppError::TooManyGreetings(n) => write!(f, "GREET_TIMES must be 1..=3, got {n}"),
    }
  }
}

/// The program: greet `name` `GREET_TIMES` times (default 1). Pure description — no I/O happens
/// until it is run with an environment that has both capabilities.
fn greet<R>(name: String) -> Effect<(), AppError, R>
where
  R: Needs<ConfigProviderService> + Needs<ConsoleService> + 'static,
{
  Config::integer("GREET_TIMES")
    .with_default(1)
    .run::<i64, AppError, R>()
    .flat_map(move |times| {
      Effect::new(move |r: &mut R| {
        if !(1..=3).contains(&times) {
          return Err(AppError::TooManyGreetings(times));
        }
        let console = Needs::<ConsoleService>::need(r).clone();
        for _ in 0..times {
          console.line(format!("hello, {name}"));
        }
        Ok(())
      })
    })
}

/// The edge: one place that decides which provider and which console the program gets.
fn edge_env(config: impl ConfigProvider + 'static, console: ConsoleService) -> Env {
  let mut env = config_env(config);
  env.insert::<Cap<ConsoleService>>(console);
  env
}

#[derive(Parser, Debug)]
#[command(name = "greet", about = "id_effect cookbook: run_blocking CLI")]
struct Cli {
  /// Who to greet.
  #[arg(long, default_value = "world")]
  name: String,
}

fn main() -> ExitCode {
  let cli = Cli::parse();
  let env = edge_env(EnvConfigProvider::from_env(), Arc::new(Stdout));
  // `run_main` prints `Err` with `Debug`; map to `String` first for a human-readable message.
  let program = greet(cli.name).map_error(|e| e.to_string());
  // `greet` is a v1 effect: bridge it, with its `Env`, at the edge.
  run_main(program.into_next_with(env), HNil, RunMainConfig::minimal())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;

  use id_effect::run_blocking;
  use id_effect_cli::exit_code_for_result;
  use id_effect_config::MapConfigProvider;

  /// Test console that records lines instead of printing them.
  #[derive(Default)]
  struct Recorder(Mutex<Vec<String>>);

  impl Console for Recorder {
    fn line(&self, text: String) {
      self.0.lock().expect("recorder").push(text);
    }
  }

  fn run_with_times(times: &str) -> (Result<(), AppError>, Vec<String>) {
    let recorder = Arc::new(Recorder::default());
    let env = edge_env(
      MapConfigProvider::from_pairs([("GREET_TIMES", times)]),
      recorder.clone(),
    );
    let result = run_blocking(greet("Ada".into()), env);
    let lines = recorder.0.lock().expect("recorder").clone();
    (result, lines)
  }

  #[test]
  fn greets_the_configured_number_of_times() {
    let (result, lines) = run_with_times("2");
    assert!(result.is_ok());
    assert_eq!(lines, ["hello, Ada", "hello, Ada"]);
  }

  #[test]
  fn invalid_config_is_a_typed_failure_with_exit_code_1() {
    let (result, lines) = run_with_times("9");
    assert!(matches!(result, Err(AppError::TooManyGreetings(9))));
    assert!(lines.is_empty());
    assert_eq!(exit_code_for_result(result), ExitCode::from(1));
  }
}
