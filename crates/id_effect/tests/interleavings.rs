//! Seeded interleaving exploration on `TestExecutor` (0.8 E2.4, scorecard test.determinism#7).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use id_effect::{
  Effect, Executor, TestExecutor, assert_interleavings, explore_interleavings, succeed, sync,
  yield_fiber, zip_par,
};

/// The order in which `n` spawned tasks are first polled.
fn spawn_order(exec: &TestExecutor, n: usize) -> Vec<usize> {
  let seen = Arc::new(Mutex::new(Vec::new()));
  for i in 0..n {
    let seen = Arc::clone(&seen);
    exec.spawn(Box::pin(async move {
      seen.lock().expect("lock").push(i);
    }));
  }
  exec.run_until_stalled();
  seen.lock().expect("lock").clone()
}

#[test]
fn new_executor_is_fifo_and_has_no_seed() {
  let exec = TestExecutor::new();
  assert_eq!(exec.seed(), None);
  assert_eq!(spawn_order(&exec, 8), (0..8).collect::<Vec<_>>());
}

#[test]
fn a_seed_selects_a_reproducible_order() {
  let exec = TestExecutor::seeded(7);
  assert_eq!(exec.seed(), Some(7));
  let first = spawn_order(&exec, 8);
  assert_eq!(spawn_order(&TestExecutor::seeded(7), 8), first);
  let mut sorted = first.clone();
  sorted.sort_unstable();
  assert_eq!(
    sorted,
    (0..8).collect::<Vec<_>>(),
    "every task runs exactly once"
  );
}

#[test]
fn different_seeds_explore_different_orders() {
  let mut orders: Vec<Vec<usize>> = (0..32)
    .map(|seed| spawn_order(&TestExecutor::seeded(seed), 6))
    .collect();
  orders.sort();
  orders.dedup();
  assert!(orders.len() > 10, "only {} distinct orders", orders.len());
}

/// A planted lost update: reads the counter, yields, then writes back what it read plus one.
/// With `pause`, it also yields before the read.
fn bump(n: &Arc<AtomicU64>, pause: bool) -> Effect<()> {
  let (r, w) = (Arc::clone(n), Arc::clone(n));
  let gap: Effect<()> = if pause { yield_fiber() } else { succeed(()) };
  gap
    .flat_map(move |()| sync(move || r.load(Ordering::SeqCst)))
    .flat_map(move |v| yield_fiber().map(move |()| v))
    .flat_map(move |v| sync(move || w.store(v + 1, Ordering::SeqCst)))
}

/// Runs two concurrent `bump`s and asserts that neither update is lost.
fn two_bumps(exec: &TestExecutor) {
  let n = Arc::new(AtomicU64::new(0));
  let h = zip_par(bump(&n, false), bump(&n, true)).fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(h.is_done());
  assert_eq!(n.load(Ordering::SeqCst), 2, "lost update");
}

#[test]
fn fifo_order_hides_the_planted_race() {
  two_bumps(&TestExecutor::new());
}

#[test]
fn exploration_finds_the_planted_race_and_the_seed_reproduces_it() {
  let failure = explore_interleavings(0..64, two_bumps).expect_err("the race is found");
  assert!(
    failure.message.contains("lost update"),
    "{}",
    failure.message
  );
  assert!(
    failure
      .to_string()
      .contains(&format!("TestExecutor::seeded({})", failure.seed))
  );

  // Every earlier seed passed; the reported one fails again, every time.
  assert_eq!(explore_interleavings(0..failure.seed, two_bumps), Ok(()));
  for _ in 0..3 {
    assert_eq!(
      explore_interleavings([failure.seed], two_bumps),
      Err(failure.clone())
    );
  }
}

/// The same two fibers with an atomic increment: no seed loses an update.
#[test]
fn exploration_passes_once_the_race_is_fixed() {
  let fixed = |exec: &TestExecutor| {
    let n = Arc::new(AtomicU64::new(0));
    let inc = |n: &Arc<AtomicU64>| -> Effect<()> {
      let n = Arc::clone(n);
      yield_fiber().flat_map(move |()| {
        sync(move || {
          n.fetch_add(1, Ordering::SeqCst);
        })
      })
    };
    let h = zip_par(inc(&n), inc(&n)).fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    assert!(h.is_done());
    assert_eq!(n.load(Ordering::SeqCst), 2);
  };
  assert_interleavings(0..256, fixed);
}

#[test]
fn exploration_stops_at_the_first_failing_seed() {
  let runs = AtomicU64::new(0);
  let failure = explore_interleavings(0..10, |exec| {
    runs.fetch_add(1, Ordering::SeqCst);
    assert_ne!(exec.seed(), Some(3), "seed three");
  })
  .unwrap_err();
  assert_eq!(failure.seed, 3);
  assert!(failure.message.contains("seed three"));
  assert_eq!(runs.load(Ordering::SeqCst), 4);
}

#[test]
#[should_panic(expected = "failed under seed 5")]
fn assert_interleavings_panics_with_the_seed() {
  assert_interleavings(0..10, |exec| assert_ne!(exec.seed(), Some(5)));
}

#[test]
fn seeded_order_still_runs_sleeps_on_virtual_time() {
  let exec = TestExecutor::seeded(1);
  let start = exec.now();
  let h = id_effect::sleep::<(), _>(std::time::Duration::from_secs(60))
    .map(|()| 1u8)
    .fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(!h.is_done());
  exec.advance(std::time::Duration::from_secs(60));
  assert!(h.is_done());
  assert_eq!(exec.now() - start, std::time::Duration::from_secs(60));
}
