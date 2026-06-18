//! Portable process spawning ([`ProcessRuntime`]) with Tokio implementation.

#![allow(clippy::new_ret_no_self, clippy::unused_unit)]
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use id_effect::kernel::Effect;
use id_effect::{Env, Needs, ProviderError, ProviderSpec};

use crate::error::ProcessError;

/// Specification for spawning a child process (minimal MVP).
#[derive(Clone, Debug)]
pub struct CommandSpec {
  /// Program path or name on `PATH`.
  pub program: OsString,
  /// Arguments (excluding argv0).
  pub args: Vec<OsString>,
  /// Optional working directory.
  pub current_dir: Option<PathBuf>,
}

impl CommandSpec {
  /// Run `program` with no extra args.
  #[inline]
  pub fn new(program: impl Into<OsString>) -> Self {
    Self {
      program: program.into(),
      args: Vec::new(),
      current_dir: None,
    }
  }

  /// Append an argument.
  #[inline]
  pub fn arg(mut self, a: impl Into<OsString>) -> Self {
    self.args.push(a.into());
    self
  }

  /// Set working directory.
  #[inline]
  pub fn dir(mut self, d: impl Into<PathBuf>) -> Self {
    self.current_dir = Some(d.into());
    self
  }
}

/// Capability: spawn and await child processes as [`Effect`] values.
#[::id_effect::capability(Arc<dyn ProcessRuntime>)]
pub trait ProcessRuntime: Send + Sync + 'static {
  /// Spawn and wait for exit status (stdout/stderr inherited by host).
  fn spawn_wait(&self, cmd: CommandSpec) -> Effect<std::process::ExitStatus, ProcessError, ()>;
}

/// Tokio-backed process runtime.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioProcessRuntime;

impl ProcessRuntime for TokioProcessRuntime {
  fn spawn_wait(&self, cmd: CommandSpec) -> Effect<std::process::ExitStatus, ProcessError, ()> {
    Effect::new_async(move |_r: &mut ()| {
      Box::pin(async move {
        let mut c = tokio::process::Command::new(&cmd.program);
        c.args(&cmd.args);
        if let Some(dir) = &cmd.current_dir {
          c.current_dir(dir);
        }
        let status = c.status().await.map_err(ProcessError::from)?;
        Ok(status)
      })
    })
  }
}

/// Default [`ProviderSpec`] for a Tokio-backed [`ProcessRuntime`].
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(ProcessRuntimeKey)]
pub struct TokioProcessRuntimeProvider;

impl TokioProcessRuntimeProvider {
  fn new() -> Arc<dyn ProcessRuntime> {
    Arc::new(TokioProcessRuntime)
  }
}

/// Spawn-wait using [`ProcessRuntimeKey`].
#[inline]
pub fn spawn_wait<R>(cmd: CommandSpec) -> Effect<std::process::ExitStatus, ProcessError, R>
where
  R: Needs<ProcessRuntimeKey> + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let rt = r.need().clone();
    let inner = rt.spawn_wait(cmd);
    Box::pin(async move { inner.run(&mut ()).await })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::ffi::OsString;

  mod command_spec {
    use super::*;

    #[test]
    fn new_sets_program_with_no_args_or_dir() {
      let c = CommandSpec::new("prog");
      assert_eq!(c.program, OsString::from("prog"));
      assert!(c.args.is_empty());
      assert!(c.current_dir.is_none());
    }

    #[test]
    fn arg_appends_in_order() {
      let c = CommandSpec::new("p").arg("a").arg("b");
      assert_eq!(c.args.len(), 2);
      assert_eq!(c.args[0], OsString::from("a"));
      assert_eq!(c.args[1], OsString::from("b"));
    }

    #[test]
    fn dir_sets_working_directory() {
      let d = PathBuf::from("/tmp/work");
      let c = CommandSpec::new("p").dir(d.clone());
      assert_eq!(c.current_dir, Some(d));
    }
  }
}
