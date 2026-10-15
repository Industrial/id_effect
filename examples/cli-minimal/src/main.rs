//! Minimal CLI: [`clap`] → [`cli_minimal::app_effect`] → [`id_effect_cli::run_main`].

use clap::Parser;
use id_effect::next::HNil;
use id_effect::next::compat::IntoNext;
use id_effect_cli::{RunMainConfig, run_main};

#[derive(Parser, Debug)]
#[command(
  name = "cli-minimal",
  about = "id_effect CLI template (see mdBook CLI chapter)"
)]
struct Cli {
  /// API token (loaded into config as `API_TOKEN`, then wrapped as [`id_effect_config::Secret`]).
  #[arg(long)]
  token: String,
}

fn main() -> std::process::ExitCode {
  let cli = Cli::parse();
  let (eff, env) = cli_minimal::app_effect(cli.token);
  // The config program is still a v1 effect: bridge it (with its env) at the edge.
  run_main(eff.into_next_with(env), HNil, RunMainConfig::with_tracing())
}
