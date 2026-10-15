//! `http_showcase`: load [`AppConfig`](http_showcase::AppConfig) from the environment, serve
//! until SIGINT / SIGTERM, then drain gracefully.
//!
//! Run: `cargo run -p http_showcase` (see the README for the variables).

use std::process::ExitCode;

use id_effect::Exit;
use id_effect_axum::EffectExecutor;

#[tokio::main]
async fn main() -> ExitCode {
  let program = http_showcase::run_until_shutdown().provide_layer(http_showcase::live_layer());
  let exit = EffectExecutor::global().run_effect(program).await;
  // Export the last batch of spans and metrics before the process exits.
  http_showcase::telemetry::Telemetry::global().flush();
  match exit {
    Exit::Success(reason) => {
      eprintln!("http_showcase: stopped ({reason:?})");
      ExitCode::SUCCESS
    }
    Exit::Failure(cause) => {
      eprintln!("http_showcase: {cause:?}");
      ExitCode::FAILURE
    }
  }
}
