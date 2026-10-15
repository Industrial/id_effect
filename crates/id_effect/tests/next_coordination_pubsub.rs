//! `c-coordination` tests for `id_effect::next::{PubSub, SubscriptionRef}`: the v1 `pubsub` and
//! `subscription_ref` unit tests ported to the next kernel (subscriptions are tied to an explicit
//! `Scope` instead of a `Scope` environment), plus back-pressure and ordering.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::Never;
use id_effect::next::{
  Effect, ExitKind, FiberHandle, PubSub, QueueError, Scope, SubscriptionRef, TestExecutor, sleep,
};
use id_effect::{Cause, Exit};

fn run<A: Send + 'static>(eff: Effect<A, Never>) -> A {
  match eff.block_on() {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("effect failed: {c:?}"),
  }
}

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

fn close(scope: &Scope) {
  run(scope.close(ExitKind::Success));
}

// ── PubSub (ported from v1 `coordination::pubsub`) ────────────────────────────────────────

mod pubsub {
  use super::*;

  #[test]
  fn subscriber_receives_all_messages() {
    let ps = PubSub::<u32>::bounded(8);
    let scope = Scope::new();
    let q = run(ps.subscribe(&scope));
    assert!(run(ps.publish(1)));
    assert!(run(ps.publish(2)));
    assert!(run(ps.publish(3)));
    for want in [1u32, 2, 3] {
      assert_eq!(drive(q.take()), Ok(want));
    }
    close(&scope);
  }

  #[test]
  fn every_subscriber_receives_every_message() {
    let ps = PubSub::<u32>::unbounded();
    let scope = Scope::new();
    let a = run(ps.subscribe(&scope));
    let b = run(ps.subscribe(&scope));
    assert_eq!(ps.subscriber_count(), 2);
    assert!(run(ps.publish(5)));
    assert_eq!(drive(a.take()), Ok(5));
    assert_eq!(drive(b.take()), Ok(5));
    close(&scope);
  }

  #[test]
  fn subscribe_auto_unsubscribes_on_scope_close() {
    let ps = PubSub::<u32>::bounded(8);
    let scope = Scope::new();
    let q = run(ps.subscribe(&scope));
    close(&scope);
    assert_eq!(ps.subscriber_count(), 0);
    assert!(
      !run(ps.publish(99)),
      "no active subscribers after scope close"
    );
    assert_eq!(
      drive(q.take()),
      Err(QueueError::Disconnected),
      "queue shut down"
    );
  }

  #[test]
  fn sliding_drops_oldest_for_slow_subscriber() {
    let ps = PubSub::<u32>::sliding(2);
    let scope = Scope::new();
    let q = run(ps.subscribe(&scope));
    for i in 1..=3u32 {
      assert!(run(ps.publish(i)));
    }
    assert_eq!(drive(q.take()), Ok(2), "value 1 was evicted");
    assert_eq!(drive(q.take()), Ok(3));
    close(&scope);
  }

  #[test]
  fn unbounded_delivers_messages_to_subscriber() {
    let ps = PubSub::<u32>::unbounded();
    let scope = Scope::new();
    let q = run(ps.subscribe(&scope));
    assert!(run(ps.publish(42)));
    assert_eq!(drive(q.take()), Ok(42));
    close(&scope);
  }

  #[test]
  fn dropping_rejects_when_no_subscriber_or_buffer_full() {
    let ps = PubSub::<u32>::dropping(2);
    assert!(!run(ps.publish(1)), "no subscribers yet");
    let scope = Scope::new();
    let q = run(ps.subscribe(&scope));
    assert!(run(ps.publish(1)));
    assert!(run(ps.publish(2)));
    assert!(!run(ps.publish(3)), "subscriber buffer full");
    assert!(run(ps.is_full()));
    assert_eq!(drive(q.take_all()), Ok(vec![1, 2]));
    close(&scope);
  }

  #[test]
  fn bounded_publish_back_pressures_until_the_subscriber_takes() {
    let ps = PubSub::<u32>::bounded(1);
    let scope = Scope::new();
    let q = run(ps.subscribe(&scope));
    assert!(run(ps.publish(1)));
    let (exec, h) = spawn_test(ps.publish::<Never, _>(2));
    assert!(!h.is_done(), "full subscriber suspends the publisher");
    assert_eq!(drive(q.take()), Ok(1));
    exec.run_until_stalled();
    assert!(matches!(take_ready(h.join_future()), Exit::Success(true)));
    assert_eq!(drive(q.take()), Ok(2));
    close(&scope);
  }

  #[test]
  fn capacity_returns_configured_value() {
    assert_eq!(PubSub::<u32>::bounded(16).capacity(), 16);
    assert_eq!(PubSub::<u32>::unbounded().capacity(), usize::MAX);
  }

  #[test]
  fn shutdown_prevents_publish_and_is_shutdown_returns_true() {
    let ps = PubSub::<u32>::bounded(8);
    assert!(!ps.is_shutdown());
    run(ps.shutdown());
    assert!(ps.is_shutdown());
    assert!(!run(ps.publish(1)));
  }

  #[test]
  fn await_shutdown_returns_immediately_when_already_shut_down() {
    let ps = PubSub::<u32>::bounded(4);
    run(ps.shutdown());
    run(ps.await_shutdown());
  }

  #[test]
  fn await_shutdown_waits_for_shutdown() {
    let ps = PubSub::<u32>::bounded(4);
    let ps2 = ps.clone();
    let eff = ps.await_shutdown::<Never, _>().fork().flat_map(move |h| {
      sleep(Duration::from_millis(10))
        .then(ps2.shutdown())
        .then(h.join())
    });
    run(eff);
    assert!(ps.is_shutdown());
  }

  #[test]
  fn size_is_empty_is_full_reflect_buffer_state() {
    let ps = PubSub::<u32>::bounded(2);
    assert_eq!(run(ps.size()), 0);
    assert!(run(ps.is_empty()));
    assert!(!run(ps.is_full()));
    let scope = Scope::new();
    let _q = run(ps.subscribe(&scope));
    run(ps.publish(1));
    assert_eq!(run(ps.size()), 1);
    assert!(!run(ps.is_empty()));
    close(&scope);
  }

  #[test]
  fn size_returns_zero_when_shut_down() {
    let ps = PubSub::<u32>::bounded(4);
    run(ps.shutdown());
    assert_eq!(run(ps.size()), 0);
    assert!(run(ps.is_empty()));
    assert!(run(ps.is_full()));
  }

  #[test]
  fn publish_all_returns_undeliverable_when_shut_down() {
    let ps = PubSub::<u32>::bounded(4);
    run(ps.shutdown());
    assert_eq!(run(ps.publish_all(vec![1u32, 2, 3])), vec![1, 2, 3]);
  }

  #[test]
  fn publish_all_delivers_messages_when_subscriber_active() {
    let ps = PubSub::<u32>::unbounded();
    let scope = Scope::new();
    let q = run(ps.subscribe(&scope));
    assert!(run(ps.publish_all(vec![10u32, 20, 30])).is_empty());
    assert_eq!(drive(q.take_all()), Ok(vec![10, 20, 30]));
    close(&scope);
  }

  #[test]
  fn subscribe_after_shutdown_gets_shut_down_queue() {
    let ps = PubSub::<u32>::bounded(4);
    run(ps.shutdown());
    let scope = Scope::new();
    let q = run(ps.subscribe(&scope));
    assert_eq!(drive(q.take()), Err(QueueError::Disconnected));
    close(&scope);
  }

  #[test]
  fn shutdown_ends_live_subscriptions_after_draining() {
    let ps = PubSub::<u32>::unbounded();
    let scope = Scope::new();
    let q = run(ps.subscribe(&scope));
    run(ps.publish(1));
    run(ps.shutdown());
    assert_eq!(drive(q.take()), Ok(1));
    assert_eq!(drive(q.take()), Err(QueueError::Disconnected));
    assert!(format!("{ps:?}").contains("shutdown: true"));
    close(&scope);
  }

  #[test]
  fn publish_returns_false_when_no_subscribers() {
    assert!(!run(PubSub::<u32>::bounded(8).publish(1)));
  }
}

// ── SubscriptionRef (ported from v1 `coordination::subscription_ref`) ─────────────────────

mod subscription_ref {
  use super::*;

  #[test]
  fn notifies_subscribers() {
    let sr = SubscriptionRef::new(0u32);
    let scope = Scope::new();
    let q = run(sr.subscribe(&scope));
    assert_eq!(drive(q.take()), Ok(0), "current value first");
    run(sr.set(1));
    assert_eq!(drive(q.take()), Ok(1));
    close(&scope);
  }

  #[test]
  fn get_reads_current() {
    let sr = SubscriptionRef::new(99u32);
    assert_eq!(run(sr.get()), 99);
    assert!(format!("{sr:?}").contains("99"));
  }

  #[test]
  fn update_publishes() {
    let sr = SubscriptionRef::new(10u32);
    let scope = Scope::new();
    let q = run(sr.subscribe(&scope));
    let _ = drive(q.take());
    run(sr.update(|n| n + 5));
    assert_eq!(drive(q.take()), Ok(15));
    close(&scope);
  }

  #[test]
  fn modify_publishes_and_returns_b() {
    let sr = SubscriptionRef::new(2u32);
    let scope = Scope::new();
    let q = run(sr.subscribe(&scope));
    let b = run(sr.modify(|n| (n * 100, n + 1)));
    assert_eq!(b, 200);
    assert_eq!(run(sr.get()), 3);
    assert_eq!(drive(q.take_all()), Ok(vec![2, 3]));
    close(&scope);
  }

  #[test]
  fn subscribers_see_every_change_in_order_without_gaps() {
    let sr = SubscriptionRef::new(0u32);
    let scope = Scope::new();
    let early = run(sr.subscribe(&scope));
    run(sr.set(1));
    run(sr.set(2));
    let late = run(sr.subscribe(&scope));
    run(sr.set(3));
    assert_eq!(drive(early.take_all()), Ok(vec![0, 1, 2, 3]));
    assert_eq!(drive(late.take_all()), Ok(vec![2, 3]));
    close(&scope);
  }
}
