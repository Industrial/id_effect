//! Minimal CLI: [`clap`] → [`cli_minimal::app_effect`] → [`id_effect_cli::run_main`]
//! (Ctrl-C interrupts the program, finalizers run, exit `130`), plus `completions <shell>`.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use id_effect_cli::completions::{self, CompletionsArgs};
use id_effect_cli::{RunMainConfig, run_main};

#[derive(Parser, Debug)]
#[command(
  name = "cli-minimal",
  about = "id_effect CLI template (see mdBook CLI chapter)",
  subcommand_negates_reqs = true
)]
struct Cli {
  /// API token (loaded into config as `API_TOKEN`, then wrapped as [`id_effect_config::Secret`]).
  #[arg(long, required = true)]
  token: Option<String>,
  #[command(subcommand)]
  command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
  /// Print a shell completion script to stdout (`cli-minimal completions bash > …`).
  Completions(CompletionsArgs),
}

fn main() -> ExitCode {
  let cli = Cli::parse();
  match (cli.command, cli.token) {
    (Some(Command::Completions(args)), _) => {
      match completions::generate::<Cli>(&args, &mut std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
          eprintln!("error: {e}");
          ExitCode::FAILURE
        }
      }
    }
    (None, Some(token)) => {
      let (eff, env) = cli_minimal::app_effect(token);
      run_main(eff, env, RunMainConfig::with_tracing())
    }
    // clap requires `--token` when no subcommand is given.
    (None, None) => ExitCode::from(2),
  }
}
