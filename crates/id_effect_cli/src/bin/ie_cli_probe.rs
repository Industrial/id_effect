//! Tiny probe binary for integration tests (exit `0` / `1` / `101`).

use clap::{Parser, ValueEnum};
use id_effect::{Effect, HNil, die, fail, succeed};
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(name = "ie_cli_probe")]
struct Args {
  /// Whether the embedded effect succeeds or fails.
  #[arg(long, value_enum)]
  mode: ProbeMode,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProbeMode {
  Ok,
  Fail,
  Die,
}

fn main() -> ExitCode {
  let args = Args::parse();
  let eff: Effect<(), String> = match args.mode {
    ProbeMode::Ok => succeed(()),
    ProbeMode::Fail => fail("boom".into()),
    ProbeMode::Die => die("bug"),
  };
  id_effect_cli::run_main(eff, HNil, id_effect_cli::RunMainConfig::minimal())
}
