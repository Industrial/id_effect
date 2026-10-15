//! `c-coordination` tests for `id_effect::next::{Queue, Channel}`: the v1 `queue` unit tests and
//! the queue-backed v1 `channel` tests ported to the next kernel, plus back-pressure and
//! interruption (an interrupted taker or offerer is removed and loses no item).

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::Never;
use id_effect::next::{Channel, Effect, FiberHandle, Queue, QueueError, TestExecutor, sleep};
use id_effect::{Cause, Exit};

fn run<A: Send + 'static>(eff: Effect<A, Never>) -> A {
  match eff.block_on() {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("effect failed: {c:?}"),
  }
}

/// Runs a queue effect (typed error `QueueError`) to a `Result`.
fn drive<A: Send + 'static>(eff: Effect<A, QueueError>) -> Result<A, QueueError> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("future is not ready"),
  }
}

fn spawn_test<A: Send + 'static, E: Send + 'static>(
  eff: Effect<A, E>,
) -> (TestExecutor, FiberHandle<A, E>) {
  let exec = TestExecutor::new();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  (exec, h)
}

// ── Queue (ported from v1 `coordination::queue`) ──────────────────────────────────────────

mod queue {
  use super::*;

  #[test]
  fn take_suspends_until_offer() {
    let q = Queue::<u32>::bounded(4);
    let q2 = q.clone();
    let eff = q.take().fork().widen().flat_map(move |h| {
      sleep(Duration::from_millis(20))
        .then(q2.offer(7))
        .then(h.join())
    });
    assert_eq!(drive(eff), Ok(7));
  }

  #[test]
  fn bounded_try_offer_returns_false_when_full() {
    let q = Queue::<u32>::bounded(1);
    assert!(run(q.try_offer(1)));
    assert!(!run(q.try_offer(2)));
    assert_eq!(drive(q.take()), Ok(1));
    assert!(run(q.try_offer(3)));
    assert_eq!(q.capacity(), Some(1));
  }

  #[test]
  fn bounded_offer_suspends_until_a_slot_frees_up() {
    let q = Queue::<u32>::bounded(1);
    assert!(run(q.offer(1)));
    let (exec, h) = spawn_test(q.offer::<Never, _>(2));
    assert!(!h.is_done(), "full bounded queue back-pressures the offer");
    assert_eq!(drive(q.take()), Ok(1));
    exec.run_until_stalled();
    assert!(matches!(take_ready(h.join_future()), Exit::Success(true)));
    assert_eq!(drive(q.take()), Ok(2));
  }

  #[test]
  fn dropping_drops_newest() {
    let q = Queue::<u32>::dropping(1);
    assert!(run(q.offer(1)));
    assert!(!run(q.offer(2)));
    assert_eq!(run(q.size()), 1);
    assert_eq!(drive(q.take()), Ok(1));
  }

  #[test]
  fn sliding_drops_oldest() {
    let q = Queue::<u32>::sliding(2);
    assert!(run(q.offer(1)));
    assert!(run(q.offer(2)));
    assert!(run(q.offer(3)));
    assert_eq!(drive(q.take()), Ok(2));
    assert_eq!(drive(q.take()), Ok(3));
  }

  #[test]
  fn await_shutdown_returns_after_shutdown() {
    let q = Queue::<u32>::unbounded();
    let q2 = q.clone();
    let eff = q.await_shutdown::<Never, _>().fork().flat_map(move |h| {
      sleep(Duration::from_millis(15))
        .then(q2.shutdown())
        .then(h.join())
    });
    run(eff);
    assert!(run(q.is_shutdown()));
    run(q.await_shutdown());
  }

  #[test]
  fn try_offer_all_retains_overflow_bounded() {
    let q = Queue::<u32>::bounded(2);
    assert_eq!(run(q.try_offer_all([1u32, 2, 3, 4])), vec![3, 4]);
    assert_eq!(drive(q.take_all()), Ok(vec![1, 2]));
  }

  #[test]
  fn offer_all_back_pressures_on_bounded() {
    let q = Queue::<u32>::bounded(2);
    let (exec, h) = spawn_test(q.offer_all::<Never, _>([1u32, 2, 3]));
    assert!(!h.is_done());
    assert_eq!(drive(q.take()), Ok(1));
    exec.run_until_stalled();
    assert!(matches!(take_ready(h.join_future()), Exit::Success(v) if v.is_empty()));
    assert_eq!(drive(q.take_all()), Ok(vec![2, 3]));
  }

  #[test]
  fn take_up_to_and_take_n() {
    let q = Queue::<u32>::bounded(10);
    run(q.offer_all([1u32, 2, 3]));
    assert_eq!(drive(q.take_up_to(2)), Ok(vec![1, 2]));
    run(q.offer_all([4u32, 5]));
    assert_eq!(drive(q.take_n(2)), Ok(vec![3, 4]));
  }

  #[test]
  fn take_between_min_max_and_edges() {
    let q = Queue::<u32>::unbounded();
    assert_eq!(drive(q.take_between(2, 1)).map(|v| v.len()), Ok(0));
    assert_eq!(drive(q.take_between(0, 0)).map(|v| v.len()), Ok(0));
    run(q.offer_all([10u32, 11, 12]));
    assert_eq!(drive(q.take_between(2, 3)).map(|v| v.len()), Ok(3));
  }

  #[test]
  fn take_n_waits_for_every_item() {
    let q = Queue::<u32>::unbounded();
    let (exec, h) = spawn_test(q.take_n(3));
    run(q.offer(1));
    run(q.offer(2));
    exec.run_until_stalled();
    assert!(!h.is_done(), "two of three");
    run(q.offer(3));
    exec.run_until_stalled();
    assert!(matches!(take_ready(h.join_future()), Exit::Success(v) if v == vec![1, 2, 3]));
  }

  #[test]
  fn poll_and_is_empty_is_full() {
    let q = Queue::<u32>::bounded(1);
    assert_eq!(drive(q.poll()), Ok(None));
    assert!(run(q.is_empty()));
    run(q.offer(7));
    assert!(run(q.is_full()));
    assert_eq!(drive(q.poll()), Ok(Some(7)));
  }

  #[test]
  fn sliding_is_full_after_capacity() {
    let q = Queue::<u32>::sliding(2);
    run(q.offer_all([1u32, 2, 3]));
    assert!(run(q.is_full()));
  }

  #[test]
  fn queue_error_display_and_debug() {
    use std::error::Error;
    let e = QueueError::Disconnected;
    assert_eq!(format!("{e}"), "queue is disconnected");
    assert!(format!("{e:?}").contains("Disconnected"));
    assert!(e.source().is_none());
  }

  #[test]
  fn take_n_and_take_up_to_zero_return_empty() {
    let q = Queue::<u32>::bounded(4);
    run(q.offer(1));
    assert_eq!(drive(q.take_n(0)), Ok(vec![]));
    assert_eq!(drive(q.take_up_to(0)), Ok(vec![]));
    assert_eq!(run(q.size()), 1, "the item stays queued");
  }

  #[test]
  fn takes_fail_when_shut_down_empty() {
    let q = Queue::<u32>::bounded(4);
    run(q.shutdown());
    assert_eq!(drive(q.take()), Err(QueueError::Disconnected));
    assert_eq!(drive(q.take_all()), Err(QueueError::Disconnected));
    assert_eq!(drive(q.take_up_to(3)), Err(QueueError::Disconnected));
    assert_eq!(drive(q.take_n(2)), Err(QueueError::Disconnected));
    assert_eq!(drive(q.poll()), Err(QueueError::Disconnected));
    assert_eq!(drive(q.take_between(1, 3)), Err(QueueError::Disconnected));
  }

  #[test]
  fn offer_after_shutdown_returns_false() {
    let q = Queue::<u32>::bounded(4);
    run(q.shutdown());
    assert!(!run(q.offer(99)));
    assert!(!run(q.try_offer(99)));
    // Ruling: the items that were not stored are returned (v1 dropped them silently).
    assert_eq!(run(q.offer_all([1u32, 2, 3])), vec![1, 2, 3]);
  }

  #[test]
  fn shutdown_wakes_suspended_takers_and_offerers() {
    let q = Queue::<u32>::bounded(1);
    run(q.offer(1));
    let (exec, offerer) = spawn_test(q.offer::<Never, _>(2));
    let empty = Queue::<u32>::unbounded();
    let (exec2, taker) = spawn_test(empty.take());
    run(q.shutdown());
    run(empty.shutdown());
    exec.run_until_stalled();
    exec2.run_until_stalled();
    assert!(matches!(
      take_ready(offerer.join_future()),
      Exit::Success(false)
    ));
    assert!(matches!(
      take_ready(taker.join_future()),
      Exit::Failure(Cause::Fail(QueueError::Disconnected))
    ));
    assert_eq!(drive(q.take()), Ok(1), "buffered items stay readable");
  }

  #[test]
  fn sizes_and_fullness_per_strategy() {
    let q = Queue::<u32>::unbounded();
    for i in 0u32..100 {
      run(q.offer(i));
    }
    assert!(!run(q.is_full()));
    assert_eq!(run(q.size()), 100);
    assert!(!run(q.is_empty()));
    assert_eq!(q.capacity(), None);

    let d = Queue::<u32>::dropping(3);
    assert!(run(d.is_empty()));
    assert_eq!(run(d.try_offer_all([10u32, 20, 30, 40])), vec![40]);
    assert_eq!(run(d.size()), 3);
    assert!(run(d.is_full()));

    let s = Queue::<u32>::sliding(3);
    assert!(
      run(s.try_offer_all([1u32, 2, 3, 4])).is_empty(),
      "sliding never retains"
    );
    assert_eq!(run(s.size()), 3);
    assert!(run(s.is_full()));
    assert_eq!(drive(s.take_all()), Ok(vec![2, 3, 4]));
  }

  #[test]
  fn is_shutdown_before_and_after() {
    let q = Queue::<u32>::bounded(2);
    assert!(!run(q.is_shutdown()));
    run(q.shutdown());
    run(q.shutdown());
    assert!(run(q.is_shutdown()));
    assert!(format!("{q:?}").contains("shutdown: true"));
  }

  #[test]
  fn take_drains_buffer_then_errors_after_shutdown() {
    let q = Queue::<u32>::bounded(4);
    run(q.offer(42));
    run(q.shutdown());
    assert_eq!(drive(q.take()), Ok(42));
    assert_eq!(drive(q.take()), Err(QueueError::Disconnected));
  }

  #[test]
  fn many_producers_and_consumers_lose_nothing() {
    let q = Queue::<u32>::bounded(4);
    let producers: Vec<Effect<(), QueueError>> = (0..4u32)
      .map(|p| {
        let q = q.clone();
        q.offer_all((0..50u32).map(move |i| p * 1000 + i))
          .map(|left| assert!(left.is_empty()))
      })
      .collect();
    let consumers: Vec<Effect<Vec<u32>, QueueError>> = (0..4).map(|_| q.take_n(50)).collect();
    let eff = id_effect::next::fork_all(consumers, id_effect::next::Concurrency::Unbounded)
      .fork()
      .widen()
      .flat_map(move |h| {
        id_effect::next::fork_all(producers, id_effect::next::Concurrency::Unbounded).then(h.join())
      });
    let mut all: Vec<u32> = drive(eff).expect("ok").into_iter().flatten().collect();
    all.sort_unstable();
    let mut want: Vec<u32> = (0..4u32)
      .flat_map(|p| (0..50).map(move |i| p * 1000 + i))
      .collect();
    want.sort_unstable();
    assert_eq!(all, want);
  }

  #[test]
  fn interrupted_taker_is_removed_and_the_next_taker_gets_the_item() {
    let q = Queue::<u32>::unbounded();
    let (exec, h) = spawn_test(q.take());
    h.interrupt_now();
    exec.run_until_stalled();
    assert!(matches!(
      take_ready(h.join_future()),
      Exit::Failure(Cause::Interrupt(_))
    ));
    run(q.offer(5));
    assert_eq!(
      run(q.size()),
      1,
      "the item was not handed to the interrupted taker"
    );
    assert_eq!(drive(q.take()), Ok(5));
  }

  #[test]
  fn interrupt_after_hand_over_puts_the_item_back() {
    let q = Queue::<u32>::unbounded();
    let (exec, h) = spawn_test(q.take());
    assert!(!h.is_done());
    // The offer hands the item to the suspended taker; the taker is interrupted before it runs
    // again, so the runloop drops its step and the item returns to the queue.
    run(q.offer(9));
    h.interrupt_now();
    exec.run_until_stalled();
    assert!(matches!(
      take_ready(h.join_future()),
      Exit::Failure(Cause::Interrupt(_))
    ));
    assert_eq!(drive(q.take()), Ok(9), "no item lost");
  }

  #[test]
  fn interrupted_partial_take_n_returns_its_items() {
    let q = Queue::<u32>::unbounded();
    let (exec, h) = spawn_test(q.take_n(3));
    run(q.offer(1));
    run(q.offer(2));
    exec.run_until_stalled();
    h.interrupt_now();
    exec.run_until_stalled();
    assert!(h.is_done());
    assert_eq!(
      drive(q.take_all()),
      Ok(vec![1, 2]),
      "partial items put back in order"
    );
  }

  #[test]
  fn interrupted_offerer_stores_nothing() {
    let q = Queue::<u32>::bounded(1);
    run(q.offer(1));
    let (exec, h) = spawn_test(q.offer::<Never, _>(2));
    h.interrupt_now();
    exec.run_until_stalled();
    assert!(h.is_done());
    assert_eq!(drive(q.take()), Ok(1));
    assert_eq!(
      drive(q.poll()),
      Ok(None),
      "the interrupted offer stored nothing"
    );
    assert!(run(q.try_offer(3)), "and left no waiter blocking the slot");
  }
}

// ── Channel (ported from the queue-backed v1 `coordination::channel` tests) ───────────────

mod channel {
  use super::*;

  #[test]
  fn read_returns_written_elements() {
    let ch = Channel::<i32>::duplex_unbounded();
    run(ch.write(1).then(ch.write(2)));
    assert_eq!(run(ch.read()), Some(1));
    assert_eq!(run(ch.read()), Some(2));
  }

  #[test]
  fn pipe_composes_two_channels() {
    let a = Channel::<i32>::duplex_unbounded().map_out(|x| x + 1);
    let b = Channel::<i32>::duplex_unbounded().map_out(|x| x * 10);
    let ab = a.pipe(b);
    run(ab.write(1));
    assert_eq!(run(ab.read()), Some(20));
  }

  #[test]
  fn from_queue_and_map_maps_input_and_drains_after_shutdown() {
    let q = Queue::<String>::unbounded();
    let ch = Channel::from_queue_and_map(q, |n: u32| format!("#{n}"));
    run(ch.write(1).then(ch.write(2)).then(ch.shutdown()));
    assert_eq!(run(ch.read()), Some("#1".to_string()));
    assert_eq!(run(ch.read()), Some("#2".to_string()));
    assert_eq!(run(ch.read()), None, "ends after shutdown and drain");
    assert!(run(ch.queue().is_shutdown()));
  }

  #[test]
  fn map_in_transforms_written_values() {
    let ch = Channel::<i32>::duplex_unbounded().map_in(|s: &'static str| s.len() as i32);
    run(ch.write("abcd"));
    assert_eq!(run(ch.read()), Some(4));
  }

  #[test]
  fn map_out_transforms_read_values() {
    let ch = Channel::<i32>::duplex_unbounded()
      .map_out(|x| x * 2)
      .map_out(|x| x + 1);
    run(ch.write(5));
    assert_eq!(run(ch.read()), Some(11));
  }

  #[test]
  fn flat_map_out_expands_and_skips_empty() {
    let ch = Channel::<i32>::duplex_unbounded()
      .flat_map_out(|x| if x == 0 { vec![] } else { vec![x, x * 10] });
    run(ch.write(0).then(ch.write(3)));
    assert_eq!(run(ch.read()), Some(3));
    assert_eq!(run(ch.read()), Some(30));
  }

  #[test]
  fn chained_flat_map_out_composes() {
    let ch = Channel::<i32>::duplex_unbounded()
      .flat_map_out(|x| vec![x, x + 1])
      .flat_map_out(|x| if x % 2 == 0 { vec![x] } else { vec![] });
    run(ch.write(1).then(ch.write(4)));
    assert_eq!(run(ch.read()), Some(2));
    assert_eq!(run(ch.read()), Some(4));
  }

  #[test]
  fn read_after_queue_shutdown_drains_then_ends() {
    let ch = Channel::<i32>::duplex_unbounded().flat_map_out(|x| vec![x, x]);
    run(ch.write(7).then(ch.shutdown()));
    assert_eq!(run(ch.read()), Some(7));
    assert_eq!(run(ch.read()), Some(7));
    assert_eq!(run(ch.read()), None);
    run(ch.write(8));
    assert_eq!(run(ch.read()), None, "writes after shutdown are dropped");
  }

  #[test]
  fn read_suspends_until_write_and_bounded_write_back_pressures() {
    let ch = Channel::<i32>::duplex_bounded(1);
    let (exec, reader) = spawn_test(ch.read::<Never, _>());
    assert!(!reader.is_done());
    run(ch.write(1));
    exec.run_until_stalled();
    assert!(matches!(
      take_ready(reader.join_future()),
      Exit::Success(Some(1))
    ));
    run(ch.write(2));
    let (exec, writer) = spawn_test(ch.write::<Never, _>(3));
    assert!(!writer.is_done(), "bounded channel is full");
    assert_eq!(run(ch.read()), Some(2));
    exec.run_until_stalled();
    assert!(writer.is_done());
    assert_eq!(run(ch.read()), Some(3));
    assert!(format!("{ch:?}").contains("Channel"));
  }
}
