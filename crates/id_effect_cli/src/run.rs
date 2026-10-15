//! `run_main` — the CLI run edge: tracing (optional), `block_on`, stderr, [`ExitCode`].

use std::fmt::{self, Debug};
use std::process::ExitCode;

use id_effect::next::IntoLocal;
use id_effect::next::compat::IntoNext;
use id_effect::{Cause, Exit, TracingConfig, install_tracing_layer};

use crate::exit_code_for_cause;

/// Options for [`run_main`].
#[derive(Clone, Debug)]
pub struct RunMainConfig {
  /// When `true`, installs the default tracing layer via [`install_tracing_layer`]
  /// ([`TracingConfig::enabled`]) before running `effect`.
  pub install_tracing: bool,
}

impl RunMainConfig {
  /// No tracing install; only runs the effect and maps its [`Exit`] to an [`ExitCode`].
  #[inline]
  pub fn minimal() -> Self {
    Self {
      install_tracing: false,
    }
  }

  /// Same as [`RunMainConfig::minimal`] but installs tracing first.
  #[inline]
  pub fn with_tracing() -> Self {
    Self {
      install_tracing: true,
    }
  }
}

/// The program edge: provide `env` to `effect`, run it to completion on the calling thread
/// (`block_on` on the default [`Runtime`](id_effect::next::Runtime)), report a failure on
/// **stderr**, and return the [`ExitCode`] for its [`Exit`] ([`exit_code_for_exit`]).
///
/// `effect` is a `Send` [`Effect`](id_effect::next::Effect) or a `!Send`
/// [`LocalEffect`](id_effect::next::LocalEffect) over the capability list `R`; pass
/// [`HNil`](id_effect::next::HNil) for an effect that needs nothing.
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
  if config.install_tracing {
    install_tracing();
  }
  #[allow(
    clippy::disallowed_methods,
    reason = "edge: `run_main` is the CLI program edge (called from `main`), the one place that blocks"
  )]
  let exit = effect.into_local().provide_env(env).block_on();
  report_exit(exit)
}

/// Installs the (still v1-backed) tracing layer through the v1 → next bridge.
fn install_tracing() {
  #[allow(
    clippy::disallowed_methods,
    reason = "edge: tracing install at the CLI program edge, before the program runs"
  )]
  let _ = install_tracing_layer(TracingConfig::enabled())
    .into_next()
    .block_on();
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
  use id_effect::next::{Effect, HCons, HNil, LocalEffect, die, fail, need, succeed, sync};

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
    fn runs_a_v1_effect_through_the_bridge() {
      let v1 = id_effect::fail::<u8, &str, ()>("v1 boom").into_next();
      assert_eq!(
        run_main(v1, HNil, RunMainConfig::minimal()),
        ExitCode::from(1u8)
      );
    }

    #[test]
    fn with_tracing_flag_does_not_panic_on_success() {
      let code = run_main(
        succeed::<(), (), HNil>(()),
        HNil,
        RunMainConfig::with_tracing(),
      );
      assert_eq!(code, ExitCode::SUCCESS);
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
