//! Ex 076 — 0.8 stream operators: `zip` / `zip_with` / `combine_latest`, `throttle` /
//! `debounce` on the fiber clock (driven here by `TestExecutor` virtual time), and `group_by`.
use std::collections::VecDeque;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::{Exit, Stream, TestExecutor, sleep, succeed};

fn main() {
  // zip pairs by position and ends with the shorter side; zip_with combines.
  let pairs = Stream::<u32>::from_iterable(1..).zip(Stream::from_iterable(['a', 'b', 'c']));
  assert_eq!(
    pairs.run_collect().block_on(),
    Exit::succeed(vec![(1, 'a'), (2, 'b'), (3, 'c')])
  );
  let sums = Stream::<u32>::from_iterable([1, 2, 3])
    .zip_with(Stream::from_iterable([10, 20, 30]), |a, b| a + b);
  assert_eq!(
    sums.run_collect().block_on(),
    Exit::succeed(vec![11, 22, 33])
  );

  // combine_latest pairs the latest of each side whenever either emits.
  let latest = Stream::<u32>::from_iterable([1, 2]).combine_latest(Stream::from_iterable(['x']));
  assert_eq!(
    latest.run_collect().block_on(),
    Exit::succeed(vec![(2, 'x')])
  );

  // throttle: 1 element per second, on the virtual clock.
  let exec = TestExecutor::new();
  let h = Stream::<u32>::from_iterable(1..=3)
    .throttle(1, Duration::from_secs(1))
    .run_collect()
    .fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(!h.is_done());
  exec.advance(Duration::from_secs(2));
  assert!(h.is_done());

  // debounce: a burst settles 300 ms after its last element.
  let mut plan: VecDeque<(u64, u32)> = VecDeque::from([(0, 1), (100, 2), (100, 3), (800, 4)]);
  let source = Stream::<u32>::from_pull(move |_| match plan.pop_front() {
    None => succeed(None),
    Some((ms, x)) => sleep(Duration::from_millis(ms)).map(move |()| Some(vec![x])),
  });
  let exec = TestExecutor::new();
  let h = source
    .debounce(Duration::from_millis(300))
    .run_collect()
    .fork_root(Arc::new(exec.clone()));
  exec.advance(Duration::from_secs(2));
  let mut done = pin!(h.join_future());
  match done.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(exit) => assert_eq!(exit, Exit::succeed(vec![3, 4])),
    Poll::Pending => panic!("debounce did not finish"),
  }

  // group_by: one stream per key, run concurrently, outputs merged.
  let per_parity = Stream::<u32>::from_iterable(1..=10).group_by(
    4,
    |n| n % 2,
    |parity, group| {
      Stream::from_effect(
        group
          .run_fold(0, |acc, n| acc + n)
          .map(move |s| vec![(parity, s)]),
      )
    },
  );
  let mut out = match per_parity.run_collect().block_on() {
    Exit::Success(v) => v,
    Exit::Failure(c) => panic!("{c:?}"),
  };
  out.sort_unstable();
  assert_eq!(out, vec![(0, 30), (1, 25)]);

  println!("076_stream_zip_time_group ok");
}
