//! The `id-effect` binary: a thin shell over [`id_effect_tool::cli::run`].

use std::process::ExitCode;

fn main() -> ExitCode {
  id_effect_tool::cli::run(
    std::env::args_os(),
    &mut std::io::stdout(),
    &mut std::io::stderr(),
  )
}
