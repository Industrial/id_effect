//! CLI ergonomics for [`id_effect::Effect`] programs: **exit code mapping**, optional
//! **[`clap`]** integration, and a small **`run_main`** helper.
//!
//! Design matches [Phase E — CLI ergonomics](https://github.com/Industrial/id_effect/blob/main/docs/effect-ts-parity/phases/phase-e-cli.md):
//! embrace **`clap`** for parsing, run the effect at the program edge with `block_on` on the
//! default [`id_effect::Runtime`], map [`Result`] / [`id_effect::Exit`] /
//! [`id_effect::Cause`] to [`std::process::ExitCode`] for `fn main() -> ExitCode`.
//!
//! ## Example
//!
//! ```rust
//! use std::process::ExitCode;
//!
//! use id_effect::{Effect, HNil, die, fail, succeed};
//! use id_effect::{Cause, Exit, FiberId};
//! use id_effect_cli::{RunMainConfig, exit_code_for_exit, run_main};
//!
//! fn greet(name: &str) -> Effect<(), String> {
//!   if name.is_empty() {
//!     fail("name must not be empty".to_string())
//!   } else {
//!     let line = format!("hello, {name}");
//!     succeed(()).map(move |()| println!("{line}"))
//!   }
//! }
//!
//! assert_eq!(run_main(greet("ada"), HNil, RunMainConfig::minimal()), ExitCode::SUCCESS);
//! assert_eq!(run_main(greet(""), HNil, RunMainConfig::minimal()), ExitCode::from(1));
//! // Defects keep their own code through `run_main` too.
//! assert_eq!(
//!   run_main(die::<(), String, HNil>("bug"), HNil, RunMainConfig::minimal()),
//!   ExitCode::from(101)
//! );
//! assert_eq!(
//!   exit_code_for_exit(Exit::<(), String>::Failure(Cause::die("bug"))),
//!   ExitCode::from(101)
//! );
//! assert_eq!(
//!   exit_code_for_exit(Exit::<(), String>::Failure(Cause::interrupt(FiberId::new(1)))),
//!   ExitCode::from(130)
//! );
//! ```
//!
//! [`examples/cli-minimal`](https://github.com/Industrial/id_effect/tree/main/examples/cli-minimal)
//! is the starter template for new CLIs.
//!
//! [`clap`]: https://docs.rs/clap

#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(feature = "completions")]
pub mod completions;
mod exit_code;
mod run;

pub use exit_code::{
  cause_max_exit_byte, exit_code_for_cause, exit_code_for_exit, exit_code_for_result,
};
pub use run::{RunMainConfig, run_main};
