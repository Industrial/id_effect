//! Bulkhead — cap concurrent in-flight effects with a semaphore.

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
}
