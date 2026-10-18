//! Private helpers on the 0.7 kernel: a fallible synchronous constructor, and (tests only) the
//! `Exit` → `Result` mapping of the synchronous / async run edges the unit tests use. The public
//! API returns `Effect`s and runs nothing itself.

#[cfg(test)]
use id_effect::{Cause, Exit};
use id_effect::{Effect, fail, succeed, suspend};
#[cfg(test)]
use std::panic::resume_unwind;

/// A lazy fallible synchronous step (the v1 `Effect::new(|_| ..)` shape).
pub(crate) fn attempt<A, E>(f: impl FnOnce() -> Result<A, E> + Send + 'static) -> Effect<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  suspend(move || match f() {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  })
}

/// Runs `eff` on the calling thread and maps its exit with [`into_result`] (unit tests).
#[cfg(test)]
#[allow(clippy::disallowed_methods, reason = "edge: unit tests run effects")]
pub(crate) fn run_sync<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  into_result(eff.block_on())
}

/// Runs `eff` on the global runtime from an async caller (unit tests).
#[cfg(test)]
pub(crate) async fn run_async<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  into_result(id_effect::Runtime::global().run(eff).await)
}

/// `Success` → `Ok`, the first typed failure → `Err`. A defect re-raises its panic (payload
/// kept) and an interrupt panics, as the v1 run edges did.
#[cfg(test)]
pub(crate) fn into_result<A, E>(exit: Exit<A, E>) -> Result<A, E> {
  match exit {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(take_fail(cause)),
  }
}

#[cfg(test)]
fn take_fail<E>(cause: Cause<E>) -> E {
  match cause {
    Cause::Fail(e) => e,
    Cause::Die(defect) => match defect.take_payload() {
      Some(payload) => resume_unwind(payload),
      None => panic!("{}", defect.message()),
    },
    Cause::Interrupt(fiber_id) => panic!("effect interrupted (fiber {fiber_id:?})"),
    Cause::Both(left, right) | Cause::Then(left, right) => {
      if left.contains(&|c: &Cause<E>| matches!(c, Cause::Fail(_))) {
        take_fail(*left)
      } else {
        take_fail(*right)
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Defect;

  #[test]
  fn attempt_maps_ok_and_err() {
    assert_eq!(run_sync(attempt(|| Ok::<u8, String>(1))), Ok(1));
    assert_eq!(
      run_sync(attempt(|| Err::<u8, String>("no".into()))),
      Err("no".into())
    );
  }

  #[test]
  fn composite_cause_yields_first_typed_failure() {
    let cause = Cause::both(Cause::die("boom"), Cause::fail(7u8));
    assert_eq!(into_result::<(), u8>(Exit::Failure(cause)), Err(7));
  }

  #[test]
  fn defect_reraises_panic() {
    let caught = std::panic::catch_unwind(|| {
      into_result::<(), u8>(Exit::Failure(Cause::Die(Defect::new("bad"))))
    });
    assert!(caught.is_err());
  }

  #[tokio::test]
  async fn run_async_drives_effect() {
    assert_eq!(run_async(succeed::<u8, String, _>(3)).await, Ok(3));
  }
}
