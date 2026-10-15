//! Portable OS subprocess spawning ([`ProcessRuntime`]) with Tokio implementation.
//!
//! The capability is [`ProcessRuntimeService`]; [`TokioProcessRuntimeProvider::layer`] and
//! [`provide_process_runtime`] are the [`Layer`]s that provide it ([`ProcessRuntimeEnv`]).
//!
//! Formerly `id_effect_platform::process`; renamed to avoid confusion with the actor processes of
//! `id_effect_process`. The deprecated `process` alias was removed in 0.7.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, from_async, hlist, need};

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

/// Handle to a running child process (supports [`child_kill`] before [`child_wait`]).
#[derive(Clone)]
pub struct ChildHandle {
  inner: Arc<Mutex<tokio::process::Child>>,
}

impl std::fmt::Debug for ChildHandle {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ChildHandle").finish_non_exhaustive()
  }
}

/// Capability: spawn and await child processes as [`Effect`] values.
///
/// The live implementation needs a tokio runtime with its IO driver enabled (child reaping), such
/// as `id_effect_tokio::TokioRuntime`.
pub trait ProcessRuntime: Send + Sync + 'static {
  /// Spawn without waiting; use [`child_wait`] or [`child_kill`].
  fn spawn(&self, cmd: CommandSpec) -> Effect<ChildHandle, ProcessError>;

  /// Spawn and wait for exit status (stdout/stderr inherited by host).
  fn spawn_wait(&self, cmd: CommandSpec) -> Effect<std::process::ExitStatus, ProcessError>;
}

/// Tokio-backed process runtime.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioProcessRuntime;

impl ProcessRuntime for TokioProcessRuntime {
  fn spawn(&self, cmd: CommandSpec) -> Effect<ChildHandle, ProcessError> {
    from_async(move || async move {
      let mut c = tokio::process::Command::new(&cmd.program);
      c.args(&cmd.args);
      if let Some(dir) = &cmd.current_dir {
        c.current_dir(dir);
      }
      let child = c.spawn().map_err(ProcessError::from)?;
      Ok(ChildHandle {
        inner: Arc::new(Mutex::new(child)),
      })
    })
  }

  fn spawn_wait(&self, cmd: CommandSpec) -> Effect<std::process::ExitStatus, ProcessError> {
    self.spawn(cmd).flat_map(child_wait)
  }
}

/// The [`ProcessRuntime`] capability: a shared handle to the installed implementation.
#[derive(Clone)]
pub struct ProcessRuntimeService(pub Arc<dyn ProcessRuntime>);

impl ProcessRuntimeService {
  /// Wrap `rt` as the capability.
  #[inline]
  pub fn new(rt: impl ProcessRuntime) -> Self {
    Self(Arc::new(rt))
  }
}

impl std::ops::Deref for ProcessRuntimeService {
  type Target = dyn ProcessRuntime;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl std::fmt::Debug for ProcessRuntimeService {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("ProcessRuntimeService")
      .field(&"<dyn ProcessRuntime>")
      .finish()
  }
}

/// The minimal typed environment holding only [`ProcessRuntimeService`] (the output of the
/// process layers).
pub type ProcessRuntimeEnv = HCons<ProcessRuntimeService, HNil>;

/// A [`ProcessRuntimeEnv`] wrapping `rt` (for `.provide_all(..)` in tests and entry points).
#[inline]
pub fn process_runtime_env(rt: impl ProcessRuntime) -> ProcessRuntimeEnv {
  hlist![ProcessRuntimeService::new(rt)]
}

/// A [`Layer`] providing `rt` as the [`ProcessRuntimeService`] capability.
#[inline]
pub fn provide_process_runtime<E: Send + 'static>(
  rt: impl ProcessRuntime,
) -> Layer<ProcessRuntimeEnv, E> {
  Layer::succeed(process_runtime_env(rt))
}

/// The Tokio-backed [`ProcessRuntime`] provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioProcessRuntimeProvider;

impl TokioProcessRuntimeProvider {
  /// The layer: a [`TokioProcessRuntime`] as the [`ProcessRuntimeService`] capability.
  #[inline]
  pub fn layer<E: Send + 'static>() -> Layer<ProcessRuntimeEnv, E> {
    provide_process_runtime(TokioProcessRuntime)
  }
}

/// Spawn using the [`ProcessRuntimeService`] in `R`.
#[inline]
pub fn spawn<R, I>(cmd: CommandSpec) -> Effect<ChildHandle, ProcessError, R>
where
  R: Selector<ProcessRuntimeService, I> + HList,
{
  need::<ProcessRuntimeService>().flat_map(move |rt| rt.spawn(cmd).narrow())
}

/// Spawn-wait using the [`ProcessRuntimeService`] in `R`.
#[inline]
pub fn spawn_wait<R, I>(cmd: CommandSpec) -> Effect<std::process::ExitStatus, ProcessError, R>
where
  R: Selector<ProcessRuntimeService, I> + HList,
{
  need::<ProcessRuntimeService>().flat_map(move |rt| rt.spawn_wait(cmd).narrow())
}

/// Await child exit (after optional [`child_kill`]).
#[inline]
pub fn child_wait<R>(handle: ChildHandle) -> Effect<std::process::ExitStatus, ProcessError, R>
where
  R: Send + Sync + 'static,
{
  from_async(move || async move {
    let mut guard = handle.inner.lock().await;
    guard.wait().await.map_err(ProcessError::from)
  })
}

/// Send kill signal to a running child (`iep-a-041` cancellation hook).
#[inline]
pub fn child_kill<R>(handle: ChildHandle) -> Effect<(), ProcessError, R>
where
  R: Send + Sync + 'static,
{
  from_async(move || async move {
    let mut guard = handle.inner.lock().await;
    guard.kill().await.map_err(ProcessError::from)
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

  mod capability {
    use super::*;
    use id_effect::Exit;

    #[test]
    fn service_is_send_sync_and_debug_hides_the_implementation() {
      fn assert_send_sync<T: Send + Sync>() {}
      assert_send_sync::<ProcessRuntimeService>();
      let s = format!("{:?}", ProcessRuntimeService::new(TokioProcessRuntime));
      assert!(s.contains("ProcessRuntimeService"), "{s}");
    }

    #[test]
    fn tokio_provider_layer_builds_the_capability() {
      let built = TokioProcessRuntimeProvider::layer::<ProcessError>()
        .build()
        .block_on();
      assert!(matches!(built, Exit::Success(_)));
    }
  }
}
