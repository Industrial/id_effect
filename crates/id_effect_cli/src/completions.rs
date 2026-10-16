//! Shell completions (feature `completions`): a `completions <shell>` subcommand in two lines.
//!
//! ```rust
//! use clap::{Parser, Subcommand};
//! use id_effect_cli::completions::{self, CompletionsArgs};
//!
//! #[derive(Parser)]
//! #[command(name = "mytool")]
//! struct Cli {
//!   #[command(subcommand)]
//!   command: Command,
//! }
//!
//! #[derive(Subcommand)]
//! enum Command {
//!   /// Print a shell completion script to stdout.
//!   Completions(CompletionsArgs),
//! }
//!
//! let cli = Cli::parse_from(["mytool", "completions", "bash"]);
//! let Command::Completions(args) = cli.command;
//! let mut script = Vec::new();
//! completions::generate::<Cli>(&args, &mut script)?;
//! assert!(String::from_utf8(script).unwrap().contains("complete -F"));
//! # Ok::<(), std::io::Error>(())
//! ```

use std::io::{self, Write};

use clap::CommandFactory;

pub use clap_complete::Shell;

/// Arguments of a `completions <shell>` subcommand (embed it with `Completions(CompletionsArgs)`).
#[derive(clap::Args, Clone, Debug)]
pub struct CompletionsArgs {
  /// Shell to generate the completion script for.
  #[arg(value_enum)]
  pub shell: Shell,
}

/// Writes the completion script of the CLI `C` for `args.shell` to `out` (usually stdout). The
/// script completes the command's binary name, falling back to its `name`.
///
/// # Errors
///
/// A failed write to `out` (for example a closed pipe).
pub fn generate<C: CommandFactory>(args: &CompletionsArgs, out: &mut dyn Write) -> io::Result<()> {
  let mut cmd = C::command();
  let bin = cmd
    .get_bin_name()
    .unwrap_or_else(|| cmd.get_name())
    .to_owned();
  // Rendered into memory first: `clap_complete` panics on a failed write (some shells even in
  // `try_generate`), so only the final `write_all` touches `out`.
  let mut script = Vec::new();
  clap_complete::generate(args.shell, &mut cmd, bin, &mut script);
  out.write_all(&script)?;
  out.flush()
}

#[cfg(test)]
mod tests {
  use super::*;
  use clap::{Parser, Subcommand};

  #[derive(Parser)]
  #[command(name = "probe-cli")]
  struct Cli {
    #[command(subcommand)]
    command: Command,
  }

  #[derive(Subcommand)]
  enum Command {
    Completions(CompletionsArgs),
  }

  fn script(shell: &str) -> String {
    let Command::Completions(args) = Cli::parse_from(["probe-cli", "completions", shell]).command;
    let mut out = Vec::new();
    generate::<Cli>(&args, &mut out).expect("write to a Vec");
    String::from_utf8(out).expect("utf-8 script")
  }

  /// A writer whose reader went away.
  struct ClosedPipe;

  impl Write for ClosedPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
      Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  mod generate {
    use super::*;

    #[test]
    fn bash_registers_a_completion_function() {
      let s = script("bash");
      assert!(s.contains("complete -F"), "{s}");
      assert!(s.contains("probe-cli"), "{s}");
    }

    #[test]
    fn zsh_and_fish_produce_scripts() {
      assert!(script("zsh").contains("#compdef probe-cli"));
      assert!(script("fish").contains("complete -c probe-cli"));
    }

    #[test]
    fn returns_a_write_error_instead_of_panicking() {
      let Command::Completions(args) =
        Cli::parse_from(["probe-cli", "completions", "fish"]).command;
      let err = generate::<Cli>(&args, &mut ClosedPipe).expect_err("closed pipe");
      assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
  }

  mod completions_args {
    use super::*;

    #[test]
    fn rejects_an_unknown_shell() {
      assert!(Cli::try_parse_from(["probe-cli", "completions", "tcsh"]).is_err());
    }
  }
}
