//! Kernel spike A: typed `R` as an HList with index inference, on an owned fiber runloop.
//!
//! Plan: `.maestro/specs/0.7/kernel-design-a-hlist.md` §5 plus the R1 shared ideas (inline
//! `Ready`, `Cause<E>` as the error channel, a one-shot exit slot, `effect_local!`/`LocalEffect`).
//! Harness: `spikes/README.md`. What was built and what was skipped: `SPIKE-NOTES.md`.
//!
//! ```
//! use kernel_a::{Effect, Never, caps, effect};
//!
//! #[derive(Debug)]
//! struct Greeting(&'static str);
//!
//! fn hello() -> Effect<String, Never, caps!(Greeting)> {
//!   effect!({ ~Greeting.0.to_string() })
//! }
//!
//! assert_eq!(hello().provide(Greeting("hi")).block_on().unwrap(), "hi");
//! ```

extern crate self as kernel_a;

mod cause;
mod effect;
mod executor;
mod fiber;
mod hlist;
mod layer;
mod local;
mod par;
mod runloop;

pub use cause::{Cause, Defect, Exit, ExitKind, Never};
pub use effect::{
  BindFuture, Ctx, Effect, Need, Restore, acquire_use_release, die, fail, from_async, need, sleep,
  succeed, suspend, sync, uninterruptible_mask, yield_now,
};
pub use executor::{BlockOnInAsync, Executor, NotLocal, Runtime, TokioExecutor};
pub use fiber::{FiberContext, FiberHandle, FiberId, JoinFuture};
pub use hlist::{Concat, Embed, HCons, HList, HNil, Here, Plucker, Sculptor, Selector, There};
pub use kernel_a_macros::{caps, effect, effect_local};
pub use layer::Layer;
pub use local::{IntoLocal, LocalEffect};
pub use par::{Admission, Concurrency, fork_all};
pub use runloop::{BoxFuture, LocalBoxFuture};

/// Macro support; not part of the API.
#[doc(hidden)]
pub mod __private {
  use std::cell::Cell;

  use crate::Never;

  /// Error widening for `~` binds: `Never` widens to anything, otherwise `E: From<E2>`.
  /// Autoref specialisation: `(&&Lift::new(e)).lift()` picks `LiftNever` for `Lift<Never>`.
  pub struct Lift<E>(Cell<Option<E>>);

  impl<E> Lift<E> {
    pub fn new(e: E) -> Self {
      Self(Cell::new(Some(e)))
    }
  }

  pub trait LiftNever {
    fn lift<E>(&self) -> E;
  }

  impl LiftNever for &Lift<Never> {
    fn lift<E>(&self) -> E {
      match self.0.take() {
        Some(n) => match n {},
        None => unreachable!(),
      }
    }
  }

  pub trait LiftFrom<E2> {
    fn lift<E: From<E2>>(&self) -> E;
  }

  impl<E2> LiftFrom<E2> for Lift<E2> {
    fn lift<E: From<E2>>(&self) -> E {
      E::from(self.0.take().expect("kernel_a: error lifted twice"))
    }
  }
}
