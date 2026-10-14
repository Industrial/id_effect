//! Bulkhead — cap concurrent in-flight effects with a semaphore.
//!
//! Admission is **fail-fast**: when every permit is taken, [`Bulkhead::with_permit`] rejects with
//! [`BulkheadError`] immediately instead of queueing. Use the core
//! [`Semaphore`] directly when callers should *wait* for a permit.

use id_effect::coordination::Semaphore;
use id_effect::failure::Or;
use id_effect::kernel::{Effect, box_future};
use id_effect::runtime::run_async;

/// Returned when the bulkhead cannot admit another effect immediately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BulkheadError;

/// Limits how many effects may run concurrently.
#[derive(Clone)]
pub struct Bulkhead {
  sem: Semaphore,
}

impl Bulkhead {
  /// Create a bulkhead allowing `max_concurrent` in-flight effects.
  pub fn make(max_concurrent: usize) -> Effect<Bulkhead, (), ()> {
    Semaphore::make(max_concurrent).map(|sem| Bulkhead { sem })
  }

  /// Run `effect` while holding one bulkhead permit.
  pub fn with_permit<A, E, R>(&self, effect: Effect<A, E, R>) -> Effect<A, Or<BulkheadError, E>, R>
  where
    A: Send + 'static,
    E: Send + Sync + 'static,
    R: Send + 'static,
  {
    let sem = self.sem.clone();
    Effect::new_async(move |r: &mut R| {
      box_future(async move {
        let permit = run_async(sem.try_acquire(), ())
          .await
          .expect("try_acquire")
          .ok_or(Or::Left(BulkheadError))?;
        let result = effect.run(r).await.map_err(Or::Right);
        drop(permit);
        result
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::kernel::succeed;
  use id_effect::runtime::run_async;

  #[tokio::test]
  async fn bulkhead_runs_effect() {
    let bh = run_async(Bulkhead::make(2), ()).await.expect("make");
    let out = run_async(bh.with_permit(succeed::<u32, (), ()>(42u32)), ())
      .await
      .expect("run");
    assert_eq!(out, 42);
  }

  type Admission = Result<u32, Or<BulkheadError, ()>>;

  /// Run `bh.with_permit(succeed(1))` from *inside* a held permit.
  fn nested(bh: &Bulkhead) -> Effect<Admission, Or<BulkheadError, ()>, ()> {
    let inner = bh.clone();
    bh.with_permit(Effect::new_async(move |_r: &mut ()| {
      box_future(
        async move { Ok(run_async(inner.with_permit(succeed::<u32, (), ()>(1u32)), ()).await) },
      )
    }))
  }

  #[tokio::test]
  async fn bulkhead_rejects_when_full() {
    let bh = run_async(Bulkhead::make(1), ()).await.expect("make");
    let inner = run_async(nested(&bh), ()).await.expect("outer admitted");
    assert_eq!(inner, Err(Or::Left(BulkheadError)));
  }

  #[tokio::test]
  async fn bulkhead_admits_up_to_capacity() {
    let bh = run_async(Bulkhead::make(2), ()).await.expect("make");
    let inner = run_async(nested(&bh), ()).await.expect("outer admitted");
    assert_eq!(inner, Ok(1));
  }

  #[tokio::test]
  async fn bulkhead_releases_permit_after_failure() {
    use id_effect::kernel::fail;
    let bh = run_async(Bulkhead::make(1), ()).await.expect("make");
    assert_eq!(
      run_async(bh.with_permit(fail::<u32, &str, ()>("boom")), ()).await,
      Err(Or::Right("boom"))
    );
    let out = run_async(bh.with_permit(succeed::<u32, (), ()>(7u32)), ())
      .await
      .expect("permit released");
    assert_eq!(out, 7);
  }
}
