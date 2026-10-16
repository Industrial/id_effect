//! `run_main` — the CLI run edge: tracing buffer (optional), Ctrl-C → interrupt (optional),
//! `block_on`, stderr, [`ExitCode`].

use std::fmt::{self, Debug};
use std::process::ExitCode;
use std::sync::Arc;

use id_effect::{Cause, Defect, Exit, FiberContext, Runtime};
use id_effect::{IntoLocal, TracingConfig, install_tracing_layer, tracing_hook};
use tokio::sync::oneshot;

use crate::exit_code_for_cause;

/// Options for [`run_main`]. Start from [`RunMainConfig::minimal`] (also the [`Default`]) or
/// [`RunMainConfig::with_tracing`] and adjust with the builder methods.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RunMainConfig {
  /// When `true`, turns on the in-process tracing buffer ([`install_tracing_layer`] with
  /// [`TracingConfig::enabled`]) and runs `effect` under its span hook ([`tracing_hook`]), so
  /// every `with_span` inside is recorded (read it back with `snapshot_tracing`).
  pub install_tracing: bool,
  /// When `true` (the default), the first Ctrl-C (`SIGINT`) interrupts the program's root fiber:
  /// finalizers run and the exit code is `130`. A second Ctrl-C exits `130` at once.
  pub interrupt_on_ctrl_c: bool,
}

impl RunMainConfig {
  /// No tracing install; runs the effect (Ctrl-C interrupts it) and maps its [`Exit`] to an
  /// [`ExitCode`].
  #[inline]
  pub fn minimal() -> Self {
    Self {
      install_tracing: false,
      interrupt_on_ctrl_c: true,
    }
  }

  /// Same as [`RunMainConfig::minimal`] but installs tracing first.
  #[inline]
  pub fn with_tracing() -> Self {
    Self::minimal().tracing(true)
  }

  /// Sets [`install_tracing`](Self::install_tracing).
  #[inline]
  #[must_use]
  pub fn tracing(mut self, on: bool) -> Self {
    self.install_tracing = on;
    self
  }

  /// Sets [`interrupt_on_ctrl_c`](Self::interrupt_on_ctrl_c); `false` keeps the process's
  /// default Ctrl-C handling (or the one the program installs).
  #[inline]
  #[must_use]
  pub fn ctrl_c(mut self, interrupt: bool) -> Self {
    self.interrupt_on_ctrl_c = interrupt;
    self
  }
}

impl Default for RunMainConfig {
  #[inline]
  fn default() -> Self {
    Self::minimal()
  }
}

/// The program edge: provide `env` to `effect`, run it to completion on the calling thread
/// (`block_on` on the default [`Runtime`](id_effect::Runtime)), report a failure on
/// **stderr**, and return the [`ExitCode`] for its [`Exit`] ([`exit_code_for_exit`]).
///
/// `effect` is a `Send` [`Effect`](id_effect::Effect) or a `!Send`
/// [`LocalEffect`](id_effect::LocalEffect) over the capability list `R`; pass
/// [`HNil`](id_effect::HNil) for an effect that needs nothing.
///
/// | outcome | stderr | exit code |
/// |---|---|---|
/// | success | – | `0` |
/// | typed failure `Fail(e)` | `error: {e:?}` | `1` |
/// | defect `Die` | `error: defect: …` | `101` |
/// | interrupt | `error: interrupted (…)` | `130` |
///
/// Typed failures are printed with [`Debug`] (covers `E = ()` and structured errors). For
/// user-facing CLI output, map errors to `String` (or a thin `Display` wrapper) first.
///
/// The program runs as an explicit root [`FiberContext`]. With
/// [`interrupt_on_ctrl_c`](RunMainConfig::interrupt_on_ctrl_c) (the default) the first Ctrl-C
/// requests its interruption, so `ensuring` / `acquire_use_release` finalizers run before the
/// `130` exit; a second Ctrl-C exits `130` immediately. The watcher stops when the program
/// ends, but the process-wide `SIGINT` handler it registered stays installed.
///
/// Call it from `main` only: it parks the calling thread, and inside an async context (a tokio
/// task) it runs nothing and exits `101`.
///
/// [`exit_code_for_exit`]: crate::exit_code_for_exit
pub fn run_main<A, E, R, F>(effect: F, env: R, config: RunMainConfig) -> ExitCode
where
  F: IntoLocal<A, E, R>,
  A: 'static,
  E: Debug + 'static,
  R: Send + Sync + 'static,
{
  let mut program = effect.into_local().provide_env(env);
  if config.install_tracing {
    install_tracing_layer(TracingConfig::enabled());
    program = program.with_span_hook(tracing_hook());
  }
  let rt = Runtime::global();
  let fiber = FiberContext::new_root(Arc::clone(rt.executor()));
  let interrupt_on_ctrl_c = config.interrupt_on_ctrl_c;
  // Everything happens on the first poll, so inside an async context nothing (not even the
  // signal handler) is installed.
  let run = async move {
    let _watch = if interrupt_on_ctrl_c {
      watch_ctrl_c(&fiber).await
    } else {
      None
    };
    program.run_fiber(fiber).await
  };
  #[allow(
    clippy::disallowed_methods,
    reason = "edge: `run_main` is the CLI program edge (called from `main`), the one place that blocks"
  )]
  let exit = rt
    .block_on_future(run)
    .unwrap_or_else(|e| Exit::Failure(Cause::Die(Defect::new(e.to_string()))));
  report_exit(exit)
}

/// Stops the Ctrl-C watcher when dropped (its receiver sees the sender go away).
type CtrlCWatch = oneshot::Sender<()>;

/// Spawns the Ctrl-C watcher for `fiber` on its executor and waits until the signal handler is
/// registered (so a Ctrl-C right after start is not lost). `None` when no handler can be
/// registered; the program then runs with the default Ctrl-C handling.
async fn watch_ctrl_c(fiber: &Arc<FiberContext>) -> Option<CtrlCWatch> {
  let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
  let (ready_tx, ready_rx) = oneshot::channel::<bool>();
  let target = Arc::clone(fiber);
  fiber.executor().spawn(Box::pin(async move {
    let Ok(mut sigint) = listen_ctrl_c() else {
      let _ = ready_tx.send(false);
      return;
    };
    let _ = ready_tx.send(true);
    let mut interrupted = false;
    loop {
      tokio::select! {
        _ = &mut stop_rx => return,
        got = sigint.recv() => {
          if got.is_none() {
            return;
          }
          if interrupted {
            std::process::exit(130);
          }
          interrupted = true;
          target.request_interrupt();
        }
      }
    }
  }));
  match ready_rx.await {
    Ok(true) => Some(stop_tx),
    _ => None,
  }
}

#[cfg(unix)]
fn listen_ctrl_c() -> std::io::Result<tokio::signal::unix::Signal> {
  tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
}

#[cfg(windows)]
fn listen_ctrl_c() -> std::io::Result<tokio::signal::windows::CtrlC> {
  tokio::signal::windows::ctrl_c()
}

/// Prints a failed [`Exit`] to stderr and maps it to an [`ExitCode`].
fn report_exit<A, E: Debug>(exit: Exit<A, E>) -> ExitCode {
  match exit {
    Exit::Success(_) => ExitCode::SUCCESS,
    Exit::Failure(cause) => {
      eprintln!("error: {}", CauseLine(&cause));
      exit_code_for_cause(cause)
    }
  }
}

/// One-line rendering of a [`Cause`] for stderr (`Fail` keeps the 0.6 `{e:?}` form).
struct CauseLine<'a, E>(&'a Cause<E>);

impl<E: Debug> fmt::Display for CauseLine<'_, E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.0 {
      Cause::Fail(e) => write!(f, "{e:?}"),
      Cause::Die(d) => write!(f, "defect: {d}"),
      Cause::Interrupt(id) => write!(f, "interrupted ({id:?})"),
      Cause::Both(l, r) => write!(f, "{} and {}", CauseLine(&**l), CauseLine(&**r)),
      Cause::Then(l, r) => write!(f, "{}, then {}", CauseLine(&**l), CauseLine(&**r)),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::FiberId;
  use id_effect::{
    Effect, EffectEvent, HCons, HNil, LocalEffect, die, fail, need, snapshot_tracing, succeed, sync,
  };

  mod run_main {
    use super::*;

    #[test]
    fn with_success_returns_success_exit_code() {
      let code = run_main(succeed::<u8, (), HNil>(9), HNil, RunMainConfig::minimal());
      assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn with_failure_returns_nonzero_exit_code() {
      let code = run_main(
        fail::<u8, &str, HNil>("boom"),
        HNil,
        RunMainConfig::minimal(),
      );
      assert_eq!(code, ExitCode::from(1u8));
    }

    #[test]
    fn with_defect_returns_101() {
      let code = run_main(die::<u8, &str, HNil>("bug"), HNil, RunMainConfig::minimal());
      assert_eq!(code, ExitCode::from(101u8));
    }

    #[test]
    fn with_panic_in_effect_returns_101() {
      let eff = sync::<u8, &str, HNil>(|| panic!("kaboom"));
      assert_eq!(
        run_main(eff, HNil, RunMainConfig::minimal()),
        ExitCode::from(101u8)
      );
    }

    #[test]
    fn provides_the_environment() {
      struct Port(u16);
      let eff: Effect<(), String, HCons<Port, HNil>> = need::<Port>().flat_map(|p| {
        if p.0 == 8080 {
          succeed(())
        } else {
          fail(format!("bad port {}", p.0))
        }
      });
      let ok = run_main(eff, HCons::new(Port(8080), HNil), RunMainConfig::minimal());
      assert_eq!(ok, ExitCode::SUCCESS);
    }

    #[test]
    fn runs_a_local_effect() {
      let rc = std::rc::Rc::new(3u8);
      let eff = LocalEffect::<u8, &str, HNil>::sync(move || *rc);
      assert_eq!(
        run_main(eff, HNil, RunMainConfig::minimal()),
        ExitCode::SUCCESS
      );
    }

    #[test]
    fn with_tracing_records_the_program_spans() {
      let eff = succeed::<(), (), HNil>(()).with_span("cli.main");
      let code = run_main(eff, HNil, RunMainConfig::with_tracing());
      assert_eq!(code, ExitCode::SUCCESS);
      let snap = snapshot_tracing();
      assert!(
        snap.effect_events.contains(&EffectEvent::Start {
          span: "cli.main".into()
        }),
        "{snap:?}"
      );
    }

    #[test]
    fn without_ctrl_c_watch_still_runs_timers() {
      let eff = id_effect::sleep::<&str, HNil>(std::time::Duration::from_millis(1));
      let code = run_main(eff, HNil, RunMainConfig::minimal().ctrl_c(false));
      assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn with_ctrl_c_watch_runs_a_suspending_effect() {
      let eff = id_effect::sleep::<&str, HNil>(std::time::Duration::from_millis(1));
      let code = run_main(eff, HNil, RunMainConfig::minimal());
      assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn inside_an_async_context_runs_nothing_and_returns_101() {
      use std::sync::atomic::{AtomicBool, Ordering};
      let ran = Arc::new(AtomicBool::new(false));
      let seen = Arc::clone(&ran);
      let (tx, rx) = std::sync::mpsc::channel();
      // A task on the default runtime's tokio executor is an async context.
      Runtime::global().executor().spawn(Box::pin(async move {
        let eff = sync::<(), &str, HNil>(move || seen.store(true, Ordering::SeqCst));
        let _ = tx.send(run_main(eff, HNil, RunMainConfig::minimal()));
      }));
      let code = rx.recv().expect("spawned task reports");
      assert_eq!(code, ExitCode::from(101u8));
      assert!(!ran.load(Ordering::SeqCst));
    }
  }

  mod run_main_config {
    use super::*;

    #[test]
    fn minimal_interrupts_on_ctrl_c_without_tracing() {
      let c = RunMainConfig::minimal();
      assert!(!c.install_tracing);
      assert!(c.interrupt_on_ctrl_c);
    }

    #[test]
    fn default_is_minimal() {
      let c = RunMainConfig::default();
      assert!(!c.install_tracing && c.interrupt_on_ctrl_c);
    }

    #[test]
    fn builder_methods_set_each_field() {
      let c = RunMainConfig::with_tracing().ctrl_c(false);
      assert!(c.install_tracing);
      assert!(!c.interrupt_on_ctrl_c);
      assert!(!c.tracing(false).install_tracing);
    }
  }

  mod cause_line {
    use super::*;

    #[test]
    fn renders_each_cause_kind() {
      let c = Cause::then(
        Cause::<&str>::fail("f"),
        Cause::both(Cause::die("d"), Cause::interrupt(FiberId::new(1))),
      );
      let line = CauseLine(&c).to_string();
      assert!(
        line.starts_with("\"f\", then defect: d and interrupted ("),
        "{line}"
      );
    }
  }
}
