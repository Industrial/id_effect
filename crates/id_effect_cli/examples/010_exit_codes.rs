//! Map effect outcomes to process exit codes with [`id_effect_cli`].
//!
//! Typed failures exit `1`, defects `101`, interrupts `130`; composite causes keep the strongest.
//!
//! Run: `cargo run -p id_effect_cli --example 010_exit_codes -- [ok|fail|die|interrupt]`

use std::process::ExitCode;

use id_effect::{Cause, Exit, FiberId, fail, succeed};
use id_effect_cli::{RunMainConfig, cause_max_exit_byte, exit_code_for_exit, run_main};

fn main() -> ExitCode {
  for (label, cause) in [
    ("fail", Cause::fail("bad input")),
    ("die", Cause::die("bug")),
    ("interrupt", Cause::interrupt(FiberId::new(1))),
    ("fail+die", Cause::both(Cause::fail("x"), Cause::die("y"))),
  ] {
    println!("{label:>9} -> exit {}", cause_max_exit_byte(&cause));
  }

  match std::env::args().nth(1).as_deref().unwrap_or("ok") {
    // `run_main` drives the effect, prints `Err` to stderr and maps the `Result`.
    "ok" => run_main(
      succeed::<_, String, ()>("done"),
      (),
      RunMainConfig::minimal(),
    ),
    "fail" => run_main(
      fail::<(), _, ()>("bad input".to_string()),
      (),
      RunMainConfig::minimal(),
    ),
    // With a full `Exit` (e.g. from a fiber join), defects and interrupts keep their codes.
    "die" => exit_code_for_exit(Exit::<(), String>::Failure(Cause::die("bug"))),
    "interrupt" => exit_code_for_exit(Exit::<(), String>::Failure(Cause::interrupt(FiberId::new(
      1,
    )))),
    other => {
      eprintln!("unknown mode {other:?}; expected ok|fail|die|interrupt");
      ExitCode::from(2)
    }
  }
}
