//! Hedged requests — race a delayed backup against a primary effect.
//!
//! Built on [`Effect::race`]: both branches run as child fibers of the caller, the first
//! **success** wins, and the loser is interrupted and awaited (its finalizers have run) before the
//! result is returned. The backup is started only once `delay` has passed on the fiber's executor
//! clock (virtual time on [`TestExecutor`](id_effect::TestExecutor)).

use std::time::Duration;

use id_effect::{Effect, sleep};

/// Runs `primary` and, after `delay`, `backup`; the first success wins and the other branch is
/// interrupted.
///
/// A failure does not end the race (Effect.ts `race` semantics): if `primary` fails before
/// `delay`, the backup still starts at `delay` and its outcome decides. When both fail, the
/// result fails with `Cause::Both(primary_cause, backup_cause)`.
///
/// ```
/// use std::time::Duration;
///
/// use id_effect::Exit;
/// use id_effect::{Effect, sleep, succeed};
/// use id_effect_resilience::hedged;
///
/// let slow: Effect<&str> = sleep(Duration::from_secs(3600)).map(|()| "primary");
/// let backup: Effect<&str> = succeed("backup");
/// assert_eq!(
///   hedged(slow, backup, Duration::from_millis(1)).block_on(),
///   Exit::Success("backup")
/// );
/// ```
pub fn hedged<A, E, R>(
  primary: Effect<A, E, R>,
  backup: Effect<A, E, R>,
  delay: Duration,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  primary.race(sleep(delay).then(backup))
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};

  use id_effect::{Cause, Exit};
  use id_effect::{TokioExecutor, fail, succeed, sync};

  use super::*;
  use crate::test_support::run_virtual;

  const HOUR: Duration = Duration::from_secs(3600);

  fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
  }

  /// `value` after `d` (virtual time), setting `ran` when it completes.
  fn after(d: Duration, value: u32, ran: &Arc<AtomicBool>) -> Effect<u32, &'static str> {
    let ran = Arc::clone(ran);
    sleep(d).then(sync(move || {
      ran.store(true, Ordering::SeqCst);
      value
    }))
  }

  #[test]
  fn fast_primary_wins_and_backup_never_starts() {
    let started = flag();
    let s = Arc::clone(&started);
    let backup = sync(move || {
      s.store(true, Ordering::SeqCst);
      2
    });
    let (exit, elapsed) = run_virtual(hedged(succeed::<u32, (), _>(1), backup, HOUR));
    assert_eq!(exit, Exit::Success(1));
    assert!(
      !started.load(Ordering::SeqCst),
      "backup interrupted during its delay"
    );
    assert_eq!(elapsed, Duration::ZERO);
  }

  #[test]
  fn slow_primary_loses_to_backup_at_the_delay() {
    let primary_done = flag();
    let backup_done = flag();
    let primary = after(Duration::from_millis(50), 1, &primary_done);
    let backup = after(Duration::ZERO, 2, &backup_done);
    let (exit, elapsed) = run_virtual(hedged(primary, backup, Duration::from_millis(5)));
    assert_eq!(exit, Exit::Success(2));
    assert_eq!(
      elapsed,
      Duration::from_millis(5),
      "decided at the hedge delay"
    );
    assert!(backup_done.load(Ordering::SeqCst));
    assert!(!primary_done.load(Ordering::SeqCst), "primary interrupted");
  }

  #[test]
  fn primary_wins_when_it_beats_the_backup() {
    let primary_done = flag();
    let backup_done = flag();
    let primary = after(Duration::from_millis(30), 1, &primary_done);
    let backup = after(Duration::from_millis(100), 2, &backup_done);
    let (exit, elapsed) = run_virtual(hedged(primary, backup, Duration::from_millis(10)));
    assert_eq!(exit, Exit::Success(1));
    assert_eq!(elapsed, Duration::from_millis(30));
    assert!(!backup_done.load(Ordering::SeqCst), "backup interrupted");
  }

  #[test]
  fn fast_primary_failure_falls_back_to_the_backup() {
    let backup_done = flag();
    let backup = after(Duration::ZERO, 2, &backup_done);
    let (exit, elapsed) = run_virtual(hedged(
      fail("primary down"),
      backup,
      Duration::from_secs(60),
    ));
    assert_eq!(exit, Exit::Success(2));
    assert_eq!(
      elapsed,
      Duration::from_secs(60),
      "the backup still waits for the delay"
    );
  }

  #[test]
  fn both_failing_combines_the_causes() {
    let (exit, _) = run_virtual(hedged::<u32, _, _>(
      fail("primary"),
      fail("backup"),
      Duration::from_millis(1),
    ));
    assert_eq!(
      exit,
      Exit::Failure(Cause::both(Cause::Fail("primary"), Cause::Fail("backup")))
    );
  }

  #[test]
  fn runs_on_tokio() {
    let rt = tokio::runtime::Builder::new_multi_thread()
      .worker_threads(2)
      .enable_time()
      .build()
      .expect("tokio runtime");
    let slow: Effect<u32, ()> = sleep(HOUR).map(|()| 1);
    let h = hedged(slow, succeed(2), Duration::from_millis(1))
      .fork_root(Arc::new(TokioExecutor::new(rt.handle().clone())));
    let exit = rt.block_on(async {
      tokio::time::timeout(Duration::from_secs(30), h.join_future())
        .await
        .expect("hedge timed out on tokio")
    });
    assert_eq!(exit, Exit::Success(2));
  }
}
