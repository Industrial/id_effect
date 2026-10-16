//! Tiny probe binary for integration tests (exit `0` / `1` / `101`, and `130` on Ctrl-C in
//! `--mode sleep`).

use clap::{Parser, ValueEnum};
use id_effect::{Effect, HNil, acquire_use_release, die, fail, sleep, succeed, sync};
use std::process::ExitCode;
use std::time::Duration;

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
  /// Prints `ready`, sleeps forever inside `acquire_use_release`; the release prints `released`.
  Sleep,
  /// Like `sleep`, but the release prints `releasing` and then hangs (a second Ctrl-C exits).
  StuckRelease,
}

fn main() -> ExitCode {
  let args = Args::parse();
  let eff: Effect<(), String> = match args.mode {
    ProbeMode::Ok => succeed(()),
    ProbeMode::Fail => fail("boom".into()),
    ProbeMode::Die => die("bug"),
    ProbeMode::Sleep => acquire_use_release(
      sync(|| println!("ready")),
      |()| sleep(Duration::MAX),
      |(), _| sync(|| println!("released")),
    ),
    ProbeMode::StuckRelease => acquire_use_release(
      sync(|| println!("ready")),
      |()| sleep(Duration::MAX),
      |(), _| sync(|| println!("releasing")).flat_map(|()| sleep(Duration::MAX)),
    ),
  };
  id_effect_cli::run_main(eff, HNil, id_effect_cli::RunMainConfig::minimal())
}
