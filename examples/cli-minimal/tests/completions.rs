//! Integration tests: `cli_minimal completions <shell>` and the `--token` run path.

use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
  Command::new(env!("CARGO_BIN_EXE_cli_minimal"))
    .args(args)
    .output()
    .expect("spawn cli_minimal")
}

mod completions {
  use super::*;

  #[test]
  fn bash_prints_a_complete_f_script() {
    let out = cli(&["completions", "bash"]);
    assert!(out.status.success(), "{out:?}");
    let script = String::from_utf8_lossy(&out.stdout);
    assert!(script.contains("complete -F"), "{script}");
  }

  #[test]
  fn zsh_and_fish_exit_zero_with_a_script() {
    for shell in ["zsh", "fish"] {
      let out = cli(&["completions", shell]);
      assert_eq!(out.status.code(), Some(0), "{shell}: {out:?}");
      assert!(!out.stdout.is_empty(), "{shell}: empty script");
    }
  }
}

mod run {
  use super::*;

  #[test]
  fn with_token_exits_zero() {
    let out = cli(&["--token", "s3cr3t"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
  }

  #[test]
  fn without_token_or_subcommand_is_a_usage_error() {
    let out = cli(&[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
  }
}
