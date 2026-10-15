//! `c-fabric-scheduler` acceptance tests for `id_effect::next::Fabric` and
//! `Concurrency::Inherit` admission (ADR 0007 Amendment 1).
//!
//! Behavioural tests run on a multi-thread `TokioExecutor` **and** on the deterministic
//! `TestExecutor` through [`on_both`]; the waiter-state tests (what is pending, what was
//! released) run on the test executor only, where "stalled" is observable.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::Never;
use id_effect::compute::{AdmissionController, ComputeFabric, ResourcePolicy};
use id_effect::next::{
  Concurrency, Effect, Executor, Fabric, TestExecutor, TokioExecutor, all, fail, for_each_par,
  sleep, succeed, sync,
};
use id_effect::{Cause, Exit};

const HOUR: Duration = Duration::from_secs(3600);

// ── drivers ───────────────────────────────────────────────────────────────────────────────

fn tokio_rt() -> &'static tokio::runtime::Runtime {
  static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
  RT.get_or_init(|| {
    tokio::runtime::Builder::new_multi_thread()
      .worker_threads(4)
      .enable_time()
      .build()
      .expect("tokio runtime")
  })
}

/// Polls `fut` once; it must be ready.
fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("future is not ready"),
  }
}

#[derive(Clone, Copy, Debug)]
enum Driver {
  Tokio,
  Test,
}

impl Driver {
  /// Runs `eff` as a root fiber and returns its exit.
  fn run<A: Send + 'static, E: Send + 'static>(self, eff: Effect<A, E>) -> Exit<A, E> {
    match self {
      Driver::Tokio => {
        let rt = tokio_rt();
        let h = eff.fork_root(Arc::new(TokioExecutor::new(rt.handle().clone())));
        rt.block_on(async {
          tokio::time::timeout(Duration::from_secs(30), h.join_future())
            .await
            .expect("root fiber timed out on tokio")
        })
      }
      Driver::Test => {
        let exec = TestExecutor::new();
        let h = eff.fork_root(Arc::new(exec.clone()));
        exec.run_until_stalled();
        while !h.is_done() {
          let deadline = exec
            .next_timer_deadline()
            .expect("root fiber stalled with no timer pending");
          exec.advance(deadline.saturating_duration_since(exec.now()));
        }
        take_ready(h.join_future())
      }
    }
  }
}

fn on_both(t: impl Fn(Driver)) {
  t(Driver::Tokio);
  t(Driver::Test);
}

// ── helpers ───────────────────────────────────────────────────────────────────────────────

fn counter() -> Arc<AtomicUsize> {
  Arc::new(AtomicUsize::new(0))
}

fn get(c: &Arc<AtomicUsize>) -> usize {
  c.load(Ordering::SeqCst)
}

/// Counts itself in and out of `in_flight` around a short sleep and records the high water.
fn tracked(
  i: usize,
  in_flight: &Arc<AtomicUsize>,
  high_water: &Arc<AtomicUsize>,
) -> Effect<usize, Never> {
  let (inc, hw, dec) = (
    Arc::clone(in_flight),
    Arc::clone(high_water),
    Arc::clone(in_flight),
  );
  sync(move || {
    let now = inc.fetch_add(1, Ordering::SeqCst) + 1;
    hw.fetch_max(now, Ordering::SeqCst);
  })
  .then(sleep(Duration::from_millis(5 + (i as u64 % 3) * 5)))
  .then(sync(move || {
    dec.fetch_sub(1, Ordering::SeqCst);
    i
  }))
}

fn controller(capacity: usize) -> Arc<AdmissionController> {
  Arc::new(AdmissionController::new(capacity, capacity))
}

// ── AC: a capacity-2 fabric caps all(Inherit) at 2 ───────────────────────────────────────

#[test]
fn capacity_2_fabric_caps_all_inherit_at_2() {
  on_both(|d| {
    let (in_flight, high_water) = (counter(), counter());
    let work: Vec<_> = (0..9)
      .map(|i| tracked(i, &in_flight, &high_water))
      .collect();
    let eff = Fabric::with_capacity(2).provide(all(work, Concurrency::Inherit));
    assert!(matches!(d.run(eff), Exit::Success(v) if v == (0..9).collect::<Vec<_>>()));
    assert_eq!(get(&high_water), 2, "{d:?}");
  });
}

#[test]
fn capacity_2_fabric_caps_for_each_par_inherit_and_gives_every_permit_back() {
  on_both(|d| {
    let ctl = controller(2);
    let (in_flight, high_water) = (counter(), counter());
    let eff = Fabric::from_controller(Arc::clone(&ctl)).provide(for_each_par(
      0..12,
      |i| tracked(i, &in_flight, &high_water),
      Concurrency::Inherit,
    ));
    assert!(matches!(d.run(eff), Exit::Success(v) if v.len() == 12));
    assert_eq!(get(&high_water), 2, "{d:?}");
    assert_eq!(ctl.available(), 2, "{d:?}: a permit leaked");
    assert_eq!(ctl.waiting(), 0, "{d:?}");
  });
}

// ── rulings: Bounded is local; no ambient fabric means Unbounded ─────────────────────────

#[test]
fn inherit_without_an_ambient_fabric_is_unbounded() {
  on_both(|d| {
    let (in_flight, high_water) = (counter(), counter());
    let work: Vec<_> = (0..9)
      .map(|i| tracked(i, &in_flight, &high_water))
      .collect();
    assert!(matches!(
      d.run(all(work, Concurrency::Inherit)),
      Exit::Success(_)
    ));
    assert_eq!(get(&high_water), 9, "{d:?}");
  });
}

#[test]
fn bounded_is_a_local_bound_that_ignores_the_fabric() {
  on_both(|d| {
    let ctl = controller(1);
    let (in_flight, high_water) = (counter(), counter());
    let work: Vec<_> = (0..9)
      .map(|i| tracked(i, &in_flight, &high_water))
      .collect();
    let eff = Fabric::from_controller(Arc::clone(&ctl)).provide(all(work, Concurrency::Bounded(3)));
    assert!(matches!(d.run(eff), Exit::Success(_)));
    assert_eq!(get(&high_water), 3, "{d:?}");
    assert_eq!(ctl.available(), 1, "{d:?}: Bounded consulted the fabric");
  });
}

#[test]
fn unbounded_ignores_the_fabric() {
  on_both(|d| {
    let (in_flight, high_water) = (counter(), counter());
    let work: Vec<_> = (0..6)
      .map(|i| tracked(i, &in_flight, &high_water))
      .collect();
    let eff = Fabric::with_capacity(1).provide(all(work, Concurrency::Unbounded));
    assert!(matches!(d.run(eff), Exit::Success(_)));
    assert_eq!(get(&high_water), 6, "{d:?}");
  });
}

// ── AC: admission is asynchronous (waker-based) ──────────────────────────────────────────

#[test]
fn a_throttled_child_waits_on_a_waker_and_starts_when_a_permit_is_released() {
  let ctl = controller(1);
  let held = take_ready(ctl.admit()); // the only permit, held outside the program
  let started = counter();
  let s = Arc::clone(&started);
  let eff: Effect<Vec<()>, Never> = Fabric::from_controller(Arc::clone(&ctl)).provide(all(
    vec![sync(move || {
      s.fetch_add(1, Ordering::SeqCst);
    })],
    Concurrency::Inherit,
  ));
  let exec = TestExecutor::new();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  // The single-threaded test executor stalled: nothing spins, the fork_all is parked on a
  // registered waker.
  assert!(!h.is_done());
  assert_eq!(get(&started), 0);
  assert_eq!(ctl.waiting(), 1);
  drop(held); // wakes the waiter
  exec.run_until_stalled();
  assert!(h.is_done());
  assert_eq!(get(&started), 1);
  assert_eq!(ctl.available(), 1);
  assert_eq!(ctl.waiting(), 0);
}

#[test]
fn a_throttled_child_holds_no_thread_on_tokio() {
  // Capacity 1 and 4 children of 20 ms: admission is serial, and the tokio workers stay free
  // (a sibling root fiber finishes while the admissions queue).
  let rt = tokio_rt();
  let exec = Arc::new(TokioExecutor::new(rt.handle().clone()));
  let work: Vec<Effect<(), Never>> = (0..4).map(|_| sleep(Duration::from_millis(20))).collect();
  let t0 = Instant::now();
  let slow = Fabric::with_capacity(1)
    .provide(all(work, Concurrency::Inherit))
    .fork_root(Arc::clone(&exec) as _);
  let quick = succeed::<u8, Never, _>(7).fork_root(exec as _);
  rt.block_on(async {
    assert!(matches!(quick.join_future().await, Exit::Success(7)));
    assert!(
      t0.elapsed() < Duration::from_millis(60),
      "quick root was starved"
    );
    assert!(matches!(slow.join_future().await, Exit::Success(v) if v.len() == 4));
  });
  assert!(
    t0.elapsed() >= Duration::from_millis(80),
    "admission was not serial"
  );
}

// ── AC: interruptible, and an interrupted waiter leaks no permit ─────────────────────────

#[test]
fn interrupting_a_waiting_fork_all_deregisters_it_and_leaks_no_permit() {
  let ctl = controller(1);
  let held = take_ready(ctl.admit());
  let eff: Effect<Vec<u8>, Never> = Fabric::from_controller(Arc::clone(&ctl))
    .provide(all(vec![succeed(1), succeed(2)], Concurrency::Inherit));
  let exec = TestExecutor::new();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert_eq!(ctl.waiting(), 1);
  h.interrupt_now();
  exec.run_until_stalled();
  assert!(h.is_done());
  assert!(matches!(
    take_ready(h.join_future()),
    Exit::Failure(Cause::Interrupt(_))
  ));
  assert_eq!(
    ctl.waiting(),
    0,
    "the interrupted waiter is still registered"
  );
  drop(held);
  assert_eq!(ctl.available(), 1, "a permit leaked");
}

#[test]
fn interrupting_admitted_children_gives_their_permits_back() {
  let ctl = controller(3);
  let work: Vec<Effect<(), Never>> = (0..5).map(|_| sleep(HOUR)).collect();
  let eff = Fabric::from_controller(Arc::clone(&ctl)).provide(all(work, Concurrency::Inherit));
  let exec = TestExecutor::new();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert_eq!(ctl.available(), 0, "3 children admitted");
  assert_eq!(ctl.waiting(), 1, "the 4th child waits");
  h.interrupt_now();
  exec.run_until_stalled();
  assert!(h.is_done());
  assert_eq!(ctl.available(), 3, "a permit leaked");
  assert_eq!(ctl.waiting(), 0);
}

#[test]
fn a_failing_child_gives_its_permit_back_and_the_queue_is_dropped() {
  on_both(|d| {
    let ctl = controller(2);
    let work: Vec<Effect<u8, &str>> = vec![
      sleep(Duration::from_millis(5)).then(succeed(1)),
      fail("boom"),
      sleep(Duration::from_millis(5)).then(succeed(3)),
      succeed(4),
    ];
    let eff = Fabric::from_controller(Arc::clone(&ctl)).provide(all(work, Concurrency::Inherit));
    assert!(
      matches!(d.run(eff), Exit::Failure(Cause::Fail("boom"))),
      "{d:?}"
    );
    assert_eq!(ctl.available(), 2, "{d:?}: a permit leaked");
    assert_eq!(ctl.waiting(), 0, "{d:?}");
  });
}

// ── nested Inherit: a held permit is lent, so small capacities cannot deadlock ───────────

#[test]
fn nested_inherit_on_a_capacity_1_fabric_completes_one_leaf_at_a_time() {
  on_both(|d| {
    let ctl = controller(1);
    let (in_flight, high_water) = (counter(), counter());
    let outer: Vec<_> = (0..3)
      .map(|o| {
        let inner: Vec<_> = (0..3)
          .map(|i| tracked(o * 3 + i, &in_flight, &high_water))
          .collect();
        all(inner, Concurrency::Inherit)
      })
      .collect();
    let eff = Fabric::from_controller(Arc::clone(&ctl)).provide(all(outer, Concurrency::Inherit));
    let Exit::Success(v) = d.run(eff) else {
      panic!("{d:?}")
    };
    assert_eq!(v.concat(), (0..9).collect::<Vec<_>>(), "{d:?}");
    assert_eq!(get(&high_water), 1, "{d:?}");
    assert_eq!(ctl.available(), 1, "{d:?}: a permit leaked");
  });
}

#[test]
fn nested_inherit_on_a_capacity_2_fabric_never_exceeds_2_leaves() {
  on_both(|d| {
    let ctl = controller(2);
    let (in_flight, high_water) = (counter(), counter());
    let outer: Vec<_> = (0..4)
      .map(|o| {
        let inner: Vec<_> = (0..4)
          .map(|i| tracked(o * 4 + i, &in_flight, &high_water))
          .collect();
        all(inner, Concurrency::Inherit)
      })
      .collect();
    let eff = Fabric::from_controller(Arc::clone(&ctl)).provide(all(outer, Concurrency::Inherit));
    assert!(
      matches!(d.run(eff), Exit::Success(v) if v.len() == 4),
      "{d:?}"
    );
    assert!(
      get(&high_water) <= 2,
      "{d:?}: {} leaves at once",
      get(&high_water)
    );
    assert_eq!(ctl.available(), 2, "{d:?}: a permit leaked");
  });
}

// ── the ambient fabric is a fiber-local value ────────────────────────────────────────────

#[test]
fn the_ambient_fabric_is_scoped_by_provide_and_inherited_by_children() {
  on_both(|d| {
    let fabric = Fabric::with_capacity(4);
    let eff: Effect<(bool, Vec<bool>, bool), Never> = Fabric::current().flat_map(move |before| {
      fabric
        .provide(all(
          (0..3).map(|_| Fabric::current().map(|f| f.is_some())),
          Concurrency::Inherit,
        ))
        .flat_map(move |inside| {
          Fabric::current().map(move |after| (before.is_none(), inside, after.is_none()))
        })
    });
    let Exit::Success((none_before, inside, none_after)) = d.run(eff) else {
      panic!("{d:?}")
    };
    assert!(none_before && none_after, "{d:?}");
    assert_eq!(inside, vec![true; 3], "{d:?}");
  });
}

#[test]
fn a_v1_compute_fabric_admits_inherit_children() {
  let compute = Arc::new(ComputeFabric::with_mock(
    ResourcePolicy::memory_cap_max_cpu(0.85),
    0.2,
    0.3,
  ));
  let cap = compute.admission().max_permits();
  on_both(|d| {
    let (in_flight, high_water) = (counter(), counter());
    let work: Vec<_> = (0..cap + 3)
      .map(|i| tracked(i, &in_flight, &high_water))
      .collect();
    let eff = Fabric::from_compute(Arc::clone(&compute)).provide(all(work, Concurrency::Inherit));
    assert!(
      matches!(d.run(eff), Exit::Success(v) if v.len() == cap + 3),
      "{d:?}"
    );
    assert!(get(&high_water) <= cap, "{d:?}");
    assert_eq!(
      compute.admission().available(),
      cap,
      "{d:?}: a permit leaked"
    );
  });
}

// ── the async admit on AdmissionController ───────────────────────────────────────────────

#[test]
fn admit_n_takes_and_returns_several_permits_and_clamps_to_max() {
  let ctl = controller(3);
  let g = take_ready(ctl.admit_n(2));
  assert_eq!(g.permits(), 2);
  assert_eq!(ctl.available(), 1);
  let big = take_ready(async {
    drop(g);
    ctl.admit_n(10).await
  });
  assert_eq!(big.permits(), 3, "clamped to max_permits");
  assert_eq!(ctl.available(), 0);
  drop(big);
  assert_eq!(ctl.available(), 3);
}

#[test]
fn rebalance_wakes_waiting_admissions() {
  use id_effect::compute::{MetricMode, MetricPolicy};
  use id_effect::compute::{MockTelemetry, RebalanceStrategy};
  let ctl = Arc::new(AdmissionController::new(0, 4));
  let mut admit = pin!(ctl.admit());
  let woke = Arc::new(Flag(AtomicUsize::new(0)));
  let waker = Waker::from(Arc::clone(&woke));
  assert!(
    admit
      .as_mut()
      .poll(&mut Context::from_waker(&waker))
      .is_pending()
  );
  assert_eq!(ctl.waiting(), 1);
  let policy = ResourcePolicy {
    memory: MetricPolicy::new(MetricMode::Max { ceiling: 0.85 }),
    cpu: MetricPolicy::new(MetricMode::Max { ceiling: 1.0 }),
    rebalance: RebalanceStrategy::ThrottleAdmission,
  };
  ctl.rebalance(&*MockTelemetry::new(0.1, 0.1), &policy, 1);
  assert!(
    woke.0.load(Ordering::SeqCst) >= 1,
    "rebalance did not wake the waiter"
  );
  assert!(
    admit
      .as_mut()
      .poll(&mut Context::from_waker(&waker))
      .is_ready()
  );
  assert_eq!(ctl.waiting(), 0);
}

struct Flag(AtomicUsize);

impl std::task::Wake for Flag {
  fn wake(self: Arc<Self>) {
    self.0.fetch_add(1, Ordering::SeqCst);
  }
}
