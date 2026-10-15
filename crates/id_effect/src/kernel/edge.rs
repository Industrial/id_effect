//! Convenience run edges over `Runtime::global()`: `Effect::block_on` and
//! `Effect::try_block_on`. Filled by `k-edge`.

use super::effect::Effect;
use crate::capability::HNil;
use crate::failure::Exit;
use crate::runtime::{BlockOnInAsync, Runtime};

#[allow(
  clippy::disallowed_methods,
  reason = "G-nb: the convenience blocking edges themselves, delegating to `Runtime`"
)]
impl<A: Send + 'static, E: Send + 'static> Effect<A, E, HNil> {
  /// Runs this effect on the process-wide default runtime ([`Runtime::global`]), parking the
  /// calling thread until it completes: a convenience for `Runtime::global().block_on(self)`.
  ///
  /// Inside an async context (a tokio task or runtime context) nothing runs and the exit is a
  /// `Die` carrying the [`BlockOnInAsync`] message; use [`Runtime::run`]`(..).await` there, or
  /// [`try_block_on`](Self::try_block_on) to get the error as a value.
  pub fn block_on(self) -> Exit<A, E> {
    Runtime::global().block_on(self)
  }

  /// [`block_on`](Self::block_on), with the async-context refusal as an error value.
  ///
  /// # Errors
  ///
  /// [`BlockOnInAsync`] when called inside an async context; nothing is run.
  pub fn try_block_on(self) -> Result<Exit<A, E>, BlockOnInAsync> {
    Runtime::global().try_block_on(self)
  }
}
