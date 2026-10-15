//! Bulkhead — cap concurrent in-flight effects with a [`Semaphore`].
//!
//! Admission is **fail-fast**: when every permit is taken, [`Bulkhead::with_permit`] rejects with
//! [`BulkheadError`] immediately instead of queueing. Use [`Semaphore::with_permit`] directly when
//! callers should *wait* for a permit.

use id_effect::Or;
use id_effect::{Effect, Semaphore, fail, sync};

/// Returned when the bulkhead cannot admit another effect immediately.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BulkheadError;

/// Limits how many effects may run concurrently. Cloning yields another handle to the same
/// bulkhead.
#[derive(Clone, Debug)]
pub struct Bulkhead {
  sem: Semaphore,
}

impl Bulkhead {
  /// A bulkhead allowing `max_concurrent` in-flight effects.
  pub fn new(max_concurrent: usize) -> Self {
    Self {
      sem: Semaphore::new(max_concurrent),
    }
  }

  /// Runs `effect` while holding one bulkhead permit, or fails with `Or::Left(BulkheadError)`
  /// at once when none is free. The permit is released however `effect` ends (success, failure,
  /// defect or interrupt); a typed failure `e` is returned as `Or::Right(e)`.
  pub fn with_permit<A, E, R>(&self, effect: Effect<A, E, R>) -> Effect<A, Or<BulkheadError, E>, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.sem.try_acquire().flat_map(move |permit| match permit {
      None => fail(Or::Left(BulkheadError)),
      Some(permit) => effect
        .map_err(Or::Right)
        .ensuring(sync(move || permit.release())),
    })
  }

  /// The number of free permits (a snapshot).
  pub fn available<E, R>(&self) -> Effect<usize, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.sem.available()
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use id_effect::{Cause, Exit};
  use id_effect::{sleep, succeed, zip_par};

  use super::*;
  use crate::test_support::{run_ok, run_virtual};

  type Admission = Result<u32, Or<BulkheadError, ()>>;

  fn admission(eff: Effect<u32, Or<BulkheadError, ()>>) -> Effect<Admission, ()> {
    eff.map(Ok).catch(|e| succeed(Err(e)))
  }

  /// `bh.with_permit(succeed(1))` run from *inside* a held permit.
  fn nested(bh: &Bulkhead) -> Effect<Admission, Or<BulkheadError, ()>> {
    bh.with_permit(admission(bh.with_permit(succeed(1))))
  }

  #[test]
  fn bulkhead_runs_effect() {
    let bh = Bulkhead::new(2);
    let out = run_ok(
      bh.with_permit(succeed::<u32, (), _>(42))
        .zip(bh.available()),
    );
    assert_eq!(out, Ok((42, 2)), "permit released after success");
  }

  #[test]
  fn bulkhead_rejects_when_full() {
    let bh = Bulkhead::new(1);
    assert_eq!(run_ok(nested(&bh)), Ok(Err(Or::Left(BulkheadError))));
  }

  #[test]
  fn bulkhead_admits_up_to_capacity() {
    let bh = Bulkhead::new(2);
    assert_eq!(run_ok(nested(&bh)), Ok(Ok(1)));
  }

  #[test]
  fn bulkhead_releases_permit_after_failure() {
    let bh = Bulkhead::new(1);
    let failed = bh.with_permit(fail::<u32, &str, _>("boom"));
    assert_eq!(run_ok(failed), Err(Or::Right("boom")));
    let out = run_ok(bh.with_permit(succeed::<u32, (), _>(7)));
    assert_eq!(out, Ok(7));
  }

  #[test]
  fn bulkhead_releases_permit_when_interrupted() {
    let bh = Bulkhead::new(1);
    let held = bh
      .with_permit(sleep::<(), _>(Duration::from_secs(3600)))
      .timeout(Duration::from_secs(1));
    let (exit, elapsed) = run_virtual(held.zip(bh.available()));
    assert!(matches!(exit, Exit::Success((None, 1))));
    assert_eq!(elapsed, Duration::from_secs(1));
  }

  #[test]
  fn concurrent_callers_beyond_capacity_are_rejected() {
    let bh = Bulkhead::new(1);
    let slow = || admission(bh.with_permit(sleep(Duration::from_secs(1)).map(|()| 1)));
    let (exit, elapsed) = run_virtual(zip_par(slow(), slow()));
    let (a, b) = match exit {
      Exit::Success(pair) => pair,
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    };
    let mut outcomes = [a, b];
    outcomes.sort_by_key(Result::is_err);
    assert_eq!(outcomes, [Ok(1), Err(Or::Left(BulkheadError))]);
    assert_eq!(elapsed, Duration::from_secs(1));
  }

  #[test]
  fn defect_releases_the_permit() {
    let bh = Bulkhead::new(1);
    let (exit, _) = run_virtual(bh.with_permit(id_effect::die::<u32, (), _>("crash")));
    assert!(matches!(exit, Exit::Failure(Cause::Die(_))));
    assert_eq!(run_ok::<_, ()>(bh.available()), Ok(1));
  }
}
