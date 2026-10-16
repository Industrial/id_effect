//! [`FakeSystem`]: a scripted [`System`] for the checks' unit tests.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::Duration;

use super::system::{Host, RunError, RunOutput, System};

/// Answers from fixed tables: variables, command lines (`program arg…`; unknown programs are
/// "not found on PATH"), a connect error and a bind error (both succeed by default), or the
/// real sockets.
#[derive(Debug, Default)]
pub(crate) struct FakeSystem {
  vars: HashMap<String, String>,
  runs: HashMap<String, Result<RunOutput, RunError>>,
  connect_error: Option<io::ErrorKind>,
  bind_error: Option<io::ErrorKind>,
  host_sockets: bool,
  /// Every `host:port` connected to or bound, in order.
  pub sockets: RefCell<Vec<String>>,
  /// Every command line run, in order.
  pub ran: RefCell<Vec<String>>,
}

impl FakeSystem {
  /// Sets the variable `key`.
  pub fn var(mut self, key: &str, value: &str) -> Self {
    self.vars.insert(key.into(), value.into());
    self
  }

  /// `line` succeeds, printing `stdout`.
  pub fn ok(self, line: &str, stdout: &str) -> Self {
    self.answer(
      line,
      Ok(RunOutput {
        success: true,
        stdout: stdout.into(),
        stderr: String::new(),
      }),
    )
  }

  /// `line` answers `result`.
  pub fn answer(mut self, line: &str, result: Result<RunOutput, RunError>) -> Self {
    self.runs.insert(line.into(), result);
    self
  }

  /// Connecting fails with `kind`.
  pub fn refuse_connect(mut self, kind: io::ErrorKind) -> Self {
    self.connect_error = Some(kind);
    self
  }

  /// Binding fails with `kind`.
  pub fn refuse_bind(mut self, kind: io::ErrorKind) -> Self {
    self.bind_error = Some(kind);
    self
  }

  /// Connects and binds for real ([`Host`]).
  pub fn host_sockets(mut self) -> Self {
    self.host_sockets = true;
    self
  }
}

impl System for FakeSystem {
  fn var(&self, key: &str) -> Option<String> {
    self.vars.get(key).cloned()
  }

  fn run(
    &self,
    _dir: &Path,
    program: &str,
    args: &[&str],
    _timeout: Option<Duration>,
  ) -> Result<RunOutput, RunError> {
    let line = std::iter::once(program)
      .chain(args.iter().copied())
      .collect::<Vec<_>>()
      .join(" ");
    self.ran.borrow_mut().push(line.clone());
    self
      .runs
      .get(&line)
      .cloned()
      .unwrap_or_else(|| Err(RunError::Spawn(format!("`{program}` not found on PATH"))))
  }

  fn connect(&self, host: &str, port: u16, timeout: Duration) -> io::Result<()> {
    self.sockets.borrow_mut().push(format!("{host}:{port}"));
    if self.host_sockets {
      return Host.connect(host, port, timeout);
    }
    match self.connect_error {
      Some(kind) => Err(io::Error::new(kind, "connection refused")),
      None => Ok(()),
    }
  }

  fn bind(&self, host: &str, port: u16) -> io::Result<()> {
    self.sockets.borrow_mut().push(format!("{host}:{port}"));
    if self.host_sockets {
      return Host.bind(host, port);
    }
    match self.bind_error {
      Some(kind) => Err(io::Error::from(kind)),
      None => Ok(()),
    }
  }
}
