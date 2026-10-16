//! Integration tests: [`ie_cli_probe`](https://github.com/Industrial/id_effect/tree/main/crates/id_effect_cli) exit status.

use std::process::Command;

fn probe() -> Command {
  Command::new(env!("CARGO_BIN_EXE_ie_cli_probe"))
}

mod ie_cli_probe_when_mode_ok {
  use super::*;

  #[test]
  fn exits_with_success_status() {
    let status = probe()
      .args(["--mode", "ok"])
      .status()
      .expect("spawn ie_cli_probe");
    assert!(status.success());
  }
}

mod ie_cli_probe_when_mode_fail {
  use super::*;

  #[test]
  fn exits_with_code_one() {
    let status = probe()
      .args(["--mode", "fail"])
      .status()
      .expect("spawn ie_cli_probe");
    assert_eq!(status.code(), Some(1));
  }
}

mod ie_cli_probe_when_mode_die {
  use super::*;

  #[test]
  fn exits_with_code_101() {
    let status = probe()
      .args(["--mode", "die"])
      .status()
      .expect("spawn ie_cli_probe");
    assert_eq!(status.code(), Some(101));
  }
}

#[cfg(unix)]
mod ie_cli_probe_when_mode_sleep_gets_sigint {
  use super::*;
  use std::io::{BufRead, BufReader, Read};
  use std::process::Stdio;

  fn spawn_until(mode: &str) -> (std::process::Child, BufReader<std::process::ChildStdout>) {
    let mut child = probe()
      .args(["--mode", mode])
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .expect("spawn ie_cli_probe");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read ready line");
    assert_eq!(line.trim(), "ready", "probe did not reach its sleep");
    (child, stdout)
  }

  /// `ready` is printed after `run_main` registered its SIGINT handler.
  fn sigint(child: &std::process::Child) {
    let kill = Command::new("kill")
      .args(["-INT", &child.id().to_string()])
      .status()
      .expect("spawn kill");
    assert!(kill.success());
  }

  #[test]
  fn second_sigint_force_exits_130_while_the_finalizer_hangs() {
    let (mut child, mut stdout) = spawn_until("stuck-release");
    sigint(&child);
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read releasing line");
    assert_eq!(line.trim(), "releasing");
    sigint(&child);
    let status = child.wait().expect("wait ie_cli_probe");
    assert_eq!(status.code(), Some(130));
  }

  #[test]
  fn runs_the_finalizer_and_exits_130() {
    let (mut child, mut stdout) = spawn_until("sleep");
    sigint(&child);

    let status = child.wait().expect("wait ie_cli_probe");
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).expect("read stdout");
    let mut stderr = String::new();
    child
      .stderr
      .take()
      .expect("stderr")
      .read_to_string(&mut stderr)
      .expect("read stderr");
    assert_eq!(status.code(), Some(130), "stderr: {stderr}");
    assert!(rest.contains("released"), "stdout: {rest}");
    assert!(stderr.contains("error: interrupted"), "stderr: {stderr}");
  }
}
