//! [`System`]: what the checks ask of the machine (variables, commands, sockets), so tests can
//! answer for it; [`Host`] is the real machine.

use std::io::{self, Read};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// A finished command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunOutput {
  /// It exited with status 0.
  pub success: bool,
  /// Its standard output (lossy UTF-8).
  pub stdout: String,
  /// Its standard error (lossy UTF-8).
  pub stderr: String,
}

/// A command that did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunError {
  /// It could not be started (not installed, not executable).
  Spawn(String),
  /// It was still running at the timeout and was killed.
  TimedOut(Duration),
}

/// The machine the checks run on.
pub(crate) trait System {
  /// The process environment variable `key`.
  fn var(&self, key: &str) -> Option<String>;

  /// Runs `program args` in `dir` with no input, killing it after `timeout` when given.
  fn run(
    &self,
    dir: &Path,
    program: &str,
    args: &[&str],
    timeout: Option<Duration>,
  ) -> Result<RunOutput, RunError>;

  /// Opens (and closes) a TCP connection to `host:port`.
  fn connect(&self, host: &str, port: u16, timeout: Duration) -> io::Result<()>;

  /// Binds (and releases) a TCP listener on `host:port`.
  fn bind(&self, host: &str, port: u16) -> io::Result<()>;
}

/// This machine.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Host;

impl System for Host {
  fn var(&self, key: &str) -> Option<String> {
    std::env::var(key).ok()
  }

  fn run(
    &self,
    dir: &Path,
    program: &str,
    args: &[&str],
    timeout: Option<Duration>,
  ) -> Result<RunOutput, RunError> {
    let mut child = Command::new(program)
      .args(args)
      .current_dir(dir)
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => RunError::Spawn(format!("`{program}` not found on PATH")),
        _ => RunError::Spawn(format!("cannot run `{program}`: {e}")),
      })?;
    // Drain both pipes on threads so a chatty child cannot block on a full pipe.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
      thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
          let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
      })
    };
    let stdout = drain(
      child
        .stdout
        .take()
        .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
      child
        .stderr
        .take()
        .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
      match child.try_wait() {
        Ok(Some(status)) => break status,
        Ok(None) => {}
        Err(e) => return Err(RunError::Spawn(format!("waiting for `{program}`: {e}"))),
      }
      if let Some(limit) = timeout
        && started.elapsed() >= limit
      {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::TimedOut(limit));
      }
      thread::sleep(Duration::from_millis(20));
    };
    Ok(RunOutput {
      success: status.success(),
      stdout: stdout.join().unwrap_or_default(),
      stderr: stderr.join().unwrap_or_default(),
    })
  }

  fn connect(&self, host: &str, port: u16, timeout: Duration) -> io::Result<()> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the host resolves to no address");
    for addr in (host, port).to_socket_addrs()? {
      match TcpStream::connect_timeout(&addr, timeout) {
        Ok(_) => return Ok(()),
        Err(e) => last = e,
      }
    }
    Err(last)
  }

  fn bind(&self, host: &str, port: u16) -> io::Result<()> {
    TcpListener::bind((host, port)).map(drop)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn run_captures_output_and_status() {
    let dir = std::env::current_dir().unwrap();
    let out = Host
      .run(&dir, "cargo", &["--version"], Some(Duration::from_secs(60)))
      .unwrap();
    assert!(out.success);
    assert!(out.stdout.starts_with("cargo "), "{out:?}");
  }

  #[test]
  fn run_reports_a_missing_program() {
    let dir = std::env::current_dir().unwrap();
    let err = Host
      .run(&dir, "id-effect-no-such-program", &[], None)
      .unwrap_err();
    assert_eq!(
      err,
      RunError::Spawn("`id-effect-no-such-program` not found on PATH".into())
    );
  }

  #[test]
  fn run_kills_a_command_at_the_timeout() {
    let dir = std::env::current_dir().unwrap();
    let limit = Duration::from_millis(100);
    let started = Instant::now();
    assert_eq!(
      Host.run(&dir, "sleep", &["10"], Some(limit)),
      Err(RunError::TimedOut(limit))
    );
    assert!(started.elapsed() < Duration::from_secs(5));
  }

  #[test]
  fn connect_and_bind_see_a_listener() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    Host
      .connect("127.0.0.1", port, Duration::from_secs(2))
      .unwrap();
    let busy = Host.bind("127.0.0.1", port).unwrap_err();
    assert_eq!(busy.kind(), io::ErrorKind::AddrInUse);
    drop(listener);
  }
}
