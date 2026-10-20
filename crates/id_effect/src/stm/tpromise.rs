//! [`TPromise`] — a transactional write-once cell.

use super::{Outcome, Stm, TRef};

/// Transactional promise (Effect.ts `TPromise`): completed at most once with a success `A` or a
/// failure `E`. [`TPromise::await_`] retries (parks) until it is completed, then yields the value
/// or fails the transaction with the error. Later completions are ignored and return `false`.
///
/// ```
/// use id_effect::Exit;
/// use id_effect::stm::{TPromise, commit};
///
/// let Exit::Success(p) = commit(TPromise::<u32, String>::make()).block_on() else { unreachable!() };
/// let p2 = p.clone();
/// let stm = p.succeed::<String>(7).flat_map(move |_| p2.await_());
/// assert!(matches!(commit(stm).block_on(), Exit::Success(7)));
/// ```
#[derive(Clone)]
pub struct TPromise<A, E> {
  inner: TRef<Option<Result<A, E>>>,
}

impl<A, E> TPromise<A, E>
where
  A: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
{
  /// A pending promise.
  pub fn make() -> Stm<TPromise<A, E>, ()> {
    TRef::make(None).map(|inner| TPromise { inner })
  }

  /// The success value, or the failure as this transaction's error; retries while pending.
  pub fn await_(&self) -> Stm<A, E> {
    let p = self.clone();
    Stm::from_fn(move |txn| match p.inner.read_txn(txn) {
      Some(Ok(a)) => Outcome::Done(a),
      Some(Err(e)) => Outcome::Fail(e),
      None => Outcome::Retry,
    })
  }

  /// The completion, or `None` while pending (never retries).
  pub fn poll<E2: Send + 'static>(&self) -> Stm<Option<Result<A, E>>, E2> {
    let p = self.clone();
    Stm::from_fn(move |txn| Outcome::Done(p.inner.read_txn(txn)))
  }

  /// Completes with `result` unless already completed; `true` when this call completed it.
  pub fn complete<E2: Send + 'static>(&self, result: Result<A, E>) -> Stm<bool, E2> {
    let p = self.clone();
    Stm::from_fn(move |txn| {
      if p.inner.read_txn(txn).is_some() {
        return Outcome::Done(false);
      }
      p.inner.set_txn(txn, Some(result.clone()));
      Outcome::Done(true)
    })
  }

  /// Completes with the success `value` (see [`TPromise::complete`]).
  pub fn succeed<E2: Send + 'static>(&self, value: A) -> Stm<bool, E2> {
    self.complete(Ok(value))
  }

  /// Completes with the failure `error` (see [`TPromise::complete`]).
  pub fn fail<E2: Send + 'static>(&self, error: E) -> Stm<bool, E2> {
    self.complete(Err(error))
  }

  /// `true` once completed.
  pub fn is_done<E2: Send + 'static>(&self) -> Stm<bool, E2> {
    self.poll().map(|r| r.is_some())
  }
}
