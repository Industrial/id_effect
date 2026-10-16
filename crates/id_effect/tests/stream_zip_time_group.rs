//! 0.8 stream operators: `zip` / `zip_with` / `combine_latest`, the fiber-clock rate operators
//! `throttle` / `throttle_enforce` / `debounce` (driven by `TestExecutor` virtual time), and
//! `group_by_key` / `group_by` (bounded buffering, backpressure). Each operator is also checked
//! to release upstream (drop it, run the finalizers of an in-flight pull) when the consumer stops
//! early.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::{Cause, Exit, Never};
use id_effect::{
  Effect, FiberHandle, Stream, TestExecutor, combine_latest, sleep, succeed, suspend, sync,
  yield_fiber,
};

fn ok<A: Send + 'static>(eff: Effect<A, Never>) -> A {
  match eff.block_on() {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("effect failed: {c:?}"),
  }
}

fn collect<A: Send + 'static>(s: Stream<A>) -> Vec<A> {
  ok(s.run_collect())
}

fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("future is not ready"),
  }
}

/// Forks `eff` on a fresh `TestExecutor` and runs it until it stalls.
fn spawn_test<A: Send + 'static, E: Send + 'static>(
  eff: Effect<A, E>,
) -> (TestExecutor, FiberHandle<A, E>) {
  let exec = TestExecutor::new();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  (exec, h)
}

fn result<A: Send + 'static, E: Send + 'static + std::fmt::Debug>(h: FiberHandle<A, E>) -> A {
  assert!(h.is_done(), "fiber still running");
  match take_ready(h.join_future()) {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("fiber failed: {c:?}"),
  }
}

fn yields<E: Send + 'static>(n: usize) -> Effect<(), E> {
  suspend(move || {
    if n == 0 {
      succeed(())
    } else {
      yield_fiber().flat_map(move |()| yields(n - 1))
    }
  })
}

/// Sets its flag when dropped: stands for a resource upstream owns.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
  fn drop(&mut self) {
    self.0.store(true, Ordering::SeqCst);
  }
}

/// An infinite source `0, 1, 2, …` that owns a [`DropFlag`].
fn owned_naturals(dropped: &Arc<AtomicBool>) -> Stream<u64> {
  let flag = DropFlag(Arc::clone(dropped));
  let mut next = 0_u64;
  Stream::from_pull(move |n| {
    let _owned = &flag;
    let batch: Vec<u64> = (next..next + n as u64).collect();
    next += n as u64;
    succeed(Some(batch))
  })
}

/// One element per pull, each after sleeping its delay (milliseconds) on the fiber clock.
fn timed(plan: Vec<(u64, u32)>) -> Stream<u32> {
  let mut plan: VecDeque<(u64, u32)> = plan.into();
  Stream::from_pull(move |_| match plan.pop_front() {
    None => succeed(None),
    Some((ms, x)) => sleep(Duration::from_millis(ms)).map(move |()| Some(vec![x])),
  })
}

/// Runs `s` collecting into a shared vector, so a test can watch the output as time advances.
fn watch(s: Stream<u32>) -> (Arc<Mutex<Vec<u32>>>, Effect<(), Never>) {
  let seen = Arc::new(Mutex::new(Vec::new()));
  let sink = Arc::clone(&seen);
  let eff = s.run_for_each(move |x| {
    let sink = Arc::clone(&sink);
    sync(move || sink.lock().unwrap().push(x))
  });
  (seen, eff)
}

fn ms(n: u64) -> Duration {
  Duration::from_millis(n)
}

// ── zip / zip_with / combine_latest ─────────────────────────────────────────────────────────

mod zip {
  use super::*;

  #[test]
  fn pairs_by_position_and_ends_with_the_shorter_side() {
    let left: Stream<u32> = Stream::from_iterable(1..=5);
    let right: Stream<char> = Stream::from_iterable(['a', 'b', 'c']);
    assert_eq!(collect(left.zip(right)), vec![(1, 'a'), (2, 'b'), (3, 'c')]);
  }

  #[test]
  fn zip_with_combines_across_uneven_chunks() {
    // The left side yields one element per pull, the right side whole chunks.
    let mut i = 0_u32;
    let left: Stream<u32> = Stream::from_pull(move |_| {
      i += 1;
      succeed((i <= 100).then(|| vec![i]))
    });
    let right: Stream<u32> = Stream::from_iterable(1..=100);
    let out = collect(left.zip_with(right, |a, b| a * 1000 + b));
    assert_eq!(out, (1..=100).map(|n| n * 1000 + n).collect::<Vec<_>>());
  }

  #[test]
  fn empty_chunks_do_not_end_the_zip() {
    let mut i = 0_u32;
    let left: Stream<u32> = Stream::from_pull(move |_| {
      i += 1;
      succeed(match i {
        n if n > 6 => None,
        n if n % 2 == 0 => Some(Vec::new()),
        n => Some(vec![n]),
      })
    });
    let out = collect(left.zip(Stream::from_iterable(10..)));
    assert_eq!(out, vec![(1, 10), (3, 11), (5, 12)]);
  }

  #[test]
  fn an_infinite_side_is_dropped_when_the_other_ends() {
    let dropped = Arc::new(AtomicBool::new(false));
    let out = collect(Stream::from_iterable([7_u64, 8]).zip(owned_naturals(&dropped)));
    assert_eq!(out, vec![(7, 0), (8, 1)]);
    assert!(dropped.load(Ordering::SeqCst));
  }

  #[test]
  fn stopping_early_drops_both_sides() {
    let (l, r) = (
      Arc::new(AtomicBool::new(false)),
      Arc::new(AtomicBool::new(false)),
    );
    let out = collect(owned_naturals(&l).zip(owned_naturals(&r)).take(3));
    assert_eq!(out, vec![(0, 0), (1, 1), (2, 2)]);
    assert!(l.load(Ordering::SeqCst) && r.load(Ordering::SeqCst));
  }

  #[test]
  fn a_failure_of_either_side_fails_the_zip() {
    let left: Stream<u32, &str> = Stream::from_iterable([1, 2]).chain(Stream::fail("left"));
    let right: Stream<u32, &str> = Stream::from_iterable(1..);
    assert_eq!(
      left.zip(right).run_collect().block_on(),
      Exit::Failure(Cause::Fail("left"))
    );
    let right: Stream<u32, &str> = Stream::fail("right");
    assert_eq!(
      Stream::from_iterable(1..)
        .zip(right)
        .run_collect()
        .block_on(),
      Exit::Failure(Cause::Fail("right"))
    );
  }

  #[test]
  fn combine_latest_method_matches_the_function() {
    let make = || {
      (
        Stream::<u32>::from_iterable([1, 2, 3]),
        Stream::<char>::from_iterable(['a', 'b']),
      )
    };
    let (a, b) = make();
    let (c, d) = make();
    assert_eq!(collect(a.combine_latest(b)), collect(combine_latest(c, d)));
  }
}

// ── throttle / throttle_enforce / debounce (virtual time) ───────────────────────────────────

mod time {
  use super::*;

  #[test]
  fn throttle_shapes_to_the_rate_on_the_virtual_clock() {
    // 2 per second: a burst of 2, then one every 500 ms.
    let (seen, eff) = watch(Stream::from_iterable(1..=5).throttle(2, Duration::from_secs(1)));
    let (exec, h) = spawn_test(eff);
    assert_eq!(*seen.lock().unwrap(), vec![1, 2]);
    exec.advance(ms(499));
    assert_eq!(seen.lock().unwrap().len(), 2);
    exec.advance(ms(1));
    assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3]);
    exec.advance(ms(500));
    assert_eq!(seen.lock().unwrap().len(), 4);
    assert!(!h.is_done());
    exec.advance(ms(500));
    assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3, 4, 5]);
    result(h);
  }

  #[test]
  fn throttle_backpressures_an_infinite_source() {
    let dropped = Arc::new(AtomicBool::new(false));
    let d2 = Arc::clone(&dropped);
    let eff = owned_naturals(&dropped)
      .throttle(1, ms(100))
      .take(3)
      .run_collect()
      .flat_map(move |v| sync(move || (v, d2.load(Ordering::SeqCst))));
    let (exec, h) = spawn_test(eff);
    exec.advance(ms(200));
    assert_eq!(
      result(h),
      (vec![0, 1, 2], true),
      "upstream released before the consumer exits"
    );
  }

  #[test]
  fn throttle_enforce_drops_elements_above_the_rate() {
    // One element every 100 ms; 2 per second admits t=100, t=200, then t=600.
    let plan = (0..10).map(|i| (100, i)).collect();
    let eff = timed(plan)
      .throttle_enforce(2, Duration::from_secs(1))
      .run_collect();
    let (exec, h) = spawn_test(eff);
    exec.advance(Duration::from_secs(2));
    assert_eq!(result(h), vec![0, 1, 5]);
  }

  #[test]
  fn debounce_emits_after_a_quiet_period_and_flushes_at_the_end() {
    // 1@0, 2@100, 3@200, 4@1000, 5@1050, end@1050.
    let plan = vec![(0, 1), (100, 2), (100, 3), (800, 4), (50, 5)];
    let (seen, eff) = watch(timed(plan).debounce(ms(300)));
    let (exec, h) = spawn_test(eff);
    exec.advance(ms(499));
    assert!(
      seen.lock().unwrap().is_empty(),
      "3 is not settled before t=500"
    );
    exec.advance(ms(1));
    assert_eq!(*seen.lock().unwrap(), vec![3]);
    exec.advance(ms(549));
    assert!(!h.is_done());
    exec.advance(ms(1));
    assert_eq!(*seen.lock().unwrap(), vec![3, 5]);
    result(h);
  }

  #[test]
  fn debounce_raises_an_upstream_failure() {
    let s: Stream<u32, &str> = Stream::from_iterable([1, 2]).chain(Stream::fail("boom"));
    let (exec, h) = spawn_test(s.debounce(ms(10)).run_collect());
    exec.advance(ms(10));
    assert!(h.is_done());
    assert_eq!(
      take_ready(h.join_future()),
      Exit::Failure(Cause::Fail("boom"))
    );
  }

  #[test]
  fn stopping_a_debounce_early_interrupts_the_in_flight_pull() {
    // After the first element, upstream's next pull waits an hour; its finalizer must run as
    // soon as the consumer has what it needs, without waiting for the hour.
    let finalized = Arc::new(AtomicBool::new(false));
    let fin = Arc::clone(&finalized);
    let mut first = true;
    let source: Stream<u32> = Stream::from_pull(move |_| {
      if std::mem::take(&mut first) {
        return succeed(Some(vec![1]));
      }
      let fin = Arc::clone(&fin);
      sleep(Duration::from_secs(3600))
        .map(|()| Some(vec![2]))
        .ensuring(sync(move || fin.store(true, Ordering::SeqCst)))
    });
    // A source whose pull never ends has no pending element to settle: use `take` on the
    // debounced stream after the first element settles.
    let f2 = Arc::clone(&finalized);
    let eff = source
      .debounce(ms(10))
      .take(1)
      .run_collect()
      .flat_map(move |v| yields(10).map(move |()| (v, f2.load(Ordering::SeqCst))));
    let (exec, h) = spawn_test(eff);
    exec.advance(ms(10));
    assert_eq!(result(h), (vec![1], true));
  }
}

// ── group_by_key / group_by ─────────────────────────────────────────────────────────────────

mod group {
  use super::*;

  #[test]
  fn group_by_key_splits_into_substreams_in_order() {
    let eff = Stream::<u32>::from_iterable(1..=20)
      .group_by_key(2, |n| n % 3)
      .map_effect(|(k, group)| group.run_collect().fork().map(move |h| (k, h)))
      .run_collect()
      .flat_map(|handles| {
        let mut all: Effect<Vec<(u32, Vec<u32>)>, Never> = succeed(Vec::new());
        for (k, h) in handles {
          all = all.flat_map(move |mut acc| {
            h.join().map(move |v| {
              acc.push((k, v));
              acc
            })
          });
        }
        all
      });
    let groups = ok(eff);
    let expected: Vec<(u32, Vec<u32>)> = [1, 2, 0]
      .into_iter()
      .map(|k| (k, (1..=20).filter(|n| n % 3 == k).collect()))
      .collect();
    assert_eq!(groups, expected);
  }

  #[test]
  fn group_by_runs_groups_concurrently_and_merges() {
    let mut out = collect(Stream::<u32>::from_iterable(1..=1000).group_by(
      8,
      |n| n % 4,
      |k, group| {
        Stream::from_effect(
          group
            .run_fold(0_u64, |acc, n| acc + u64::from(n))
            .map(move |s| vec![(k, s)]),
        )
      },
    ));
    out.sort_unstable();
    let expected: Vec<(u32, u64)> = (0..4)
      .map(|k| (k, (1..=1000_u64).filter(|n| n % 4 == u64::from(k)).sum()))
      .collect();
    assert_eq!(out, expected);
  }

  #[test]
  fn group_by_preserves_order_within_a_group() {
    let mut out = collect(Stream::<u32>::from_iterable(0..300).group_by(
      2,
      |n| n % 3,
      |_, group| group.map(|n| (n % 3, n)),
    ));
    for k in 0..3 {
      let mine: Vec<u32> = out
        .iter()
        .filter(|(g, _)| *g == k)
        .map(|(_, n)| *n)
        .collect();
      assert_eq!(mine, (0..300).filter(|n| n % 3 == k).collect::<Vec<_>>());
    }
    out.sort_unstable();
    assert_eq!(out.len(), 300);
  }

  #[test]
  fn an_undrained_group_backpressures_the_source() {
    // One key, buffer 2: while the substream is not pulled, the router stops after its first
    // upstream chunk instead of draining the infinite source.
    let produced = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&produced);
    let mut next = 0_u64;
    let source: Stream<u64> = Stream::from_pull(move |n| {
      counter.fetch_add(n as u64, Ordering::SeqCst);
      let batch: Vec<u64> = (next..next + n as u64).collect();
      next += n as u64;
      succeed(Some(batch))
    });
    let p = Arc::clone(&produced);
    let eff = source
      .group_by_key(2, |_| 0_u8)
      .next_chunk()
      .flat_map(move |first| {
        let (chunk, outer) = first.expect("one group");
        let (_, group) = chunk.into_vec().remove(0);
        yields(100).flat_map(move |()| {
          let stalled_at = p.load(Ordering::SeqCst);
          group.take(5).run_collect().map(move |v| {
            drop(outer);
            (stalled_at, v)
          })
        })
      });
    let (_exec, h) = spawn_test(eff);
    let (stalled_at, first_five) = result(h);
    assert_eq!(
      stalled_at, 64,
      "one upstream chunk, then the full group blocks the router"
    );
    assert_eq!(first_five, vec![0, 1, 2, 3, 4]);
  }

  #[test]
  fn a_dropped_substream_discards_its_key() {
    // Group 0 is dropped at once; groups 1 and 2 still see all their elements.
    let mut out = collect(Stream::<u32>::from_iterable(0..90).group_by(
      1,
      |n| n % 3,
      |k, group| if k == 0 { Stream::empty() } else { group },
    ));
    out.sort_unstable();
    assert_eq!(out, (0..90).filter(|n| n % 3 != 0).collect::<Vec<_>>());
  }

  #[test]
  fn an_upstream_failure_fails_the_outer_stream() {
    let s: Stream<u32, &str> = Stream::from_iterable(0..10).chain(Stream::fail("boom"));
    let out = s.group_by(4, |n| n % 2, |_, g| g).run_collect().block_on();
    assert_eq!(out, Exit::Failure(Cause::Fail("boom")));
    let s: Stream<u32, &str> = Stream::from_iterable(0..10).chain(Stream::fail("boom"));
    let outer = s
      .group_by_key(4, |n| n % 2)
      .map(|(k, _)| k)
      .run_collect()
      .block_on();
    assert_eq!(outer, Exit::Failure(Cause::Fail("boom")));
  }

  #[test]
  fn a_group_failure_fails_the_result() {
    let out = Stream::<u32, &str>::from_iterable(0..10)
      .group_by(
        4,
        |n| n % 2,
        |k, g| if k == 1 { Stream::fail("odd") } else { g },
      )
      .run_collect()
      .block_on();
    assert_eq!(out, Exit::Failure(Cause::Fail("odd")));
  }

  #[test]
  fn stopping_early_stops_the_router_and_drops_upstream() {
    let dropped = Arc::new(AtomicBool::new(false));
    let d2 = Arc::clone(&dropped);
    let eff = owned_naturals(&dropped)
      .group_by(4, |n| n % 2, |_, g| g)
      .take(3)
      .run_collect()
      .flat_map(move |v| yields(20).map(move |()| (v.len(), d2.load(Ordering::SeqCst))));
    let (_exec, h) = spawn_test(eff);
    assert_eq!(result(h), (3, true));
  }

  #[test]
  fn group_by_is_lazy_until_pulled() {
    let dropped = Arc::new(AtomicBool::new(false));
    let produced = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&produced);
    let flag = DropFlag(Arc::clone(&dropped));
    let source: Stream<u64> = Stream::from_pull(move |_| {
      let _owned = &flag;
      counter.fetch_add(1, Ordering::SeqCst);
      succeed(None)
    });
    let s = source.group_by(4, |n| *n, |_, g| g);
    assert_eq!(produced.load(Ordering::SeqCst), 0);
    drop(s);
    assert!(dropped.load(Ordering::SeqCst));
  }
}
