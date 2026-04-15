//! Synchronous and async execution functions: `run_blocking`, `run_async`, `Never` (§ 6.2).
//!
//! These are the lowest-level interpreters for [`crate::kernel::Effect`], composing directly
//! from Stratum 2 primitives without any fiber or runtime-trait dependency.

use core::convert::Infallible;

use crate::kernel::{BoxFuture, Effect};

/// Effect.ts-style uninhabited error marker for infallible runtime operations.
pub type Never = Infallible;

/// Run an effect to completion on the current thread using a tight poll loop.
///
/// The future from [`Effect::run`] is polled with `Waker::noop`. If a step returns
/// `Poll::Pending`, this implementation calls `std::thread::yield_now` and polls again — there is
/// no real async driver, so effects that genuinely need to await I/O or other wakeups may spin or
/// stall. For those cases use [`run_async`] (or an executor) instead.
#[inline]
pub fn run_blocking<A, E, R>(effect: Effect<A, E, R>, mut env: R) -> Result<A, E>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  // `Effect::run` already returns `Pin<Box<dyn Future>>`; poll it directly — do not `Box::pin`
  // again (that would allocate a second box around the first).
  let mut fut: BoxFuture<'_, Result<A, E>> = effect.run(&mut env);
  let waker = std::task::Waker::noop();
  let mut cx = std::task::Context::from_waker(waker);
  loop {
    match fut.as_mut().poll(&mut cx) {
      std::task::Poll::Ready(output) => return output,
      std::task::Poll::Pending => std::thread::yield_now(),
    }
  }
}

/// Run an effect to completion using the async executor (`.await` on [`Effect::run`]).
#[inline]
pub async fn run_async<A, E, R>(effect: Effect<A, E, R>, mut env: R) -> Result<A, E>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  effect.run(&mut env).await
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::kernel::succeed;

  mod run_blocking {
    use super::*;

    #[test]
    fn with_success_effect_returns_ok_value() {
      let result = run_blocking(succeed::<u8, (), ()>(9), ());
      assert_eq!(result, Ok(9));
    }

    #[test]
    fn completes_flat_mapped_effect_without_double_boxing() {
      let eff = crate::kernel::succeed::<u8, &'static str, ()>(3)
        .flat_map(|n| crate::kernel::succeed(n + 1));
      assert_eq!(run_blocking(eff, ()), Ok(4));
    }

    #[test]
    fn with_failure_effect_returns_err_value() {
      let result = run_blocking(crate::kernel::fail::<u8, &str, ()>("boom"), ());
      assert_eq!(result, Err("boom"));
    }
  }

  mod run_async {
    use super::*;

    #[test]
    fn with_success_effect_returns_ok_value() {
      let async_out = pollster::block_on(run_async(succeed::<u8, (), ()>(11), ()));
      assert_eq!(async_out, Ok(11));
    }
  }
}
