//! Faults injected into the `SqlClient` and HTTP test doubles, on the virtual clock.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Executor, Exit, Schedule, TestExecutor, TestRandom};
use id_effect_sql::{SqlClient, SqlError, SqlRow, TestSqlClient};

use super::{Fault, FaultKind, FaultPlan};

const Q: &str = "SELECT id FROM accounts";

fn take_ready<F: Future>(fut: F) -> F::Output {
  match pin!(fut)
    .as_mut()
    .poll(&mut Context::from_waker(Waker::noop()))
  {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("root fiber is not done"),
  }
}

/// Runs `eff` on a fresh `TestExecutor`, advancing from deadline to deadline; its exit and the
/// virtual time it took.
fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
  let exec = TestExecutor::new();
  let start = exec.now();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  while !h.is_done() {
    let deadline = exec
      .next_timer_deadline()
      .expect("stalled with no timer pending");
    exec.advance(deadline.saturating_duration_since(exec.now()));
  }
  (take_ready(h.join_future()), exec.now() - start)
}

/// A connected SQL double answering `Q` with one row.
fn sql_double() -> TestSqlClient {
  let client = TestSqlClient::new();
  client.script_query(Q, vec![SqlRow::from_pairs([("id", 1i64)])]);
  assert!(client.connect().block_on().into_result().is_ok());
  client
}

fn chaos_plan(seed: u64) -> FaultPlan<SqlError> {
  FaultPlan::seeded(seed)
    .with(0.3, Fault::delay(Duration::from_millis(250)))
    .with(0.2, Fault::fail(SqlError::connection_failed("injected")))
    .with(0.1, Fault::timeout())
}

/// 64 calls on a fresh SQL double under `chaos_plan(seed)`, each bounded by 1 s: the plan's
/// history and each call's outcome with the virtual time it took.
fn chaos_run(seed: u64) -> (Vec<Option<FaultKind>>, Vec<(String, Duration)>) {
  let client = sql_double();
  let plan = chaos_plan(seed);
  let outcomes = (0..64)
    .map(|_| {
      let (exit, took) = run_virtual(
        plan
          .inject(client.query(Q, &[]))
          .timeout(Duration::from_secs(1)),
      );
      let outcome = match exit {
        Exit::Success(Some(rows)) => format!("rows {}", rows.len()),
        Exit::Success(None) => "timed out".to_owned(),
        Exit::Failure(cause) => format!("{cause:?}"),
      };
      (outcome, took)
    })
    .collect();
  (plan.history(), outcomes)
}

#[test]
fn same_seed_same_fault_schedule() {
  let (history, outcomes) = chaos_run(7);
  assert_eq!(chaos_run(7), (history.clone(), outcomes.clone()));
  assert_ne!(chaos_run(8).0, history, "another seed, another schedule");

  assert_eq!(history.len(), 64, "one decision per call");
  for kind in [
    None,
    Some(FaultKind::Delay(Duration::from_millis(250))),
    Some(FaultKind::Fail),
    Some(FaultKind::Timeout),
  ] {
    assert!(history.contains(&kind), "{kind:?} drawn in 64 calls");
  }
  // Each outcome is its call's fault.
  for (kind, (outcome, took)) in history.iter().zip(&outcomes) {
    let expected = match kind {
      None => ("rows 1", Duration::ZERO),
      Some(FaultKind::Delay(d)) => ("rows 1", *d),
      Some(FaultKind::Fail) => (
        "Fail(ConnectionFailed { message: \"injected\" })",
        Duration::ZERO,
      ),
      Some(FaultKind::Timeout) => ("timed out", Duration::from_secs(1)),
    };
    assert_eq!((outcome.as_str(), *took), expected);
  }
}

#[test]
fn delay_advances_virtual_clock_only() {
  let client = sql_double();
  let plan = FaultPlan::seeded(1).with(1.0, Fault::delay(Duration::from_secs(30)));
  let exec = TestExecutor::new();
  let wall = Instant::now();
  let h = plan
    .inject(client.query(Q, &[]))
    .fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(!h.is_done());
  exec.advance(Duration::from_secs(30) - Duration::from_nanos(1));
  assert!(!h.is_done(), "still delayed a nanosecond before 30 s");
  assert!(
    client.recorded_queries().is_empty(),
    "the call runs after the delay"
  );
  exec.advance(Duration::from_nanos(1));
  assert!(h.is_done());
  let rows = take_ready(h.join_future()).into_result().expect("rows");
  assert_eq!(rows[0].get::<i64>("id"), Ok(1));
  assert_eq!(client.recorded_queries().len(), 1);
  assert!(
    wall.elapsed() < Duration::from_secs(5),
    "30 s of latency took {:?} of wall time",
    wall.elapsed()
  );
  assert_eq!(
    plan.history(),
    [Some(FaultKind::Delay(Duration::from_secs(30)))]
  );
}

#[test]
fn retry_policy_survives_injected_latency() {
  let client = sql_double();
  // Most attempts are 5 s late: past the caller's 1 s deadline.
  let plan = FaultPlan::seeded(3).with(0.7, Fault::delay(Duration::from_secs(5)));
  let attempt = {
    let (client, plan) = (client.clone(), plan.clone());
    move || {
      plan
        .inject(client.query(Q, &[]))
        .timeout_fail(Duration::from_secs(1), || {
          SqlError::connection_failed("timed out")
        })
    }
  };
  let backoff = Duration::from_millis(100);
  let eff = Effect::retry(
    attempt,
    Schedule::exponential(backoff).intersect(Schedule::recurs(10)),
  );
  let (exit, took) = run_virtual(eff);

  let rows = exit.into_result().expect("a retry gets through");
  assert_eq!(rows.len(), 1);
  let history = plan.history();
  let (last, delayed) = history.split_last().expect("attempts");
  assert_eq!(*last, None, "the attempt that succeeded was not delayed");
  assert!(!delayed.is_empty(), "seed 3 delays the first attempt");
  assert!(
    delayed
      .iter()
      .all(|k| *k == Some(FaultKind::Delay(Duration::from_secs(5))))
  );
  // Each timed-out attempt costs its 1 s deadline plus the backoff 100 ms * 2^i.
  let k = delayed.len() as u32;
  let expected =
    Duration::from_secs(k.into()) + (0..k).map(|i| backoff * 2u32.pow(i)).sum::<Duration>();
  assert_eq!(took, expected);
  assert_eq!(
    client.recorded_queries().len(),
    1,
    "timed-out attempts never ran"
  );
}

#[test]
fn fail_and_timeout_faults_do_not_run_the_call() {
  let client = sql_double();
  let (exit, took) =
    run_virtual(Fault::fail(SqlError::connection_failed("injected")).apply(client.query(Q, &[])));
  let injected = SqlError::connection_failed("injected");
  assert!(
    matches!(exit, Exit::Failure(Cause::Fail(ref e)) if *e == injected),
    "{exit:?}"
  );
  assert_eq!(took, Duration::ZERO);
  let (exit, took) = run_virtual(
    Fault::timeout()
      .apply(client.query(Q, &[]))
      .timeout(Duration::from_secs(2)),
  );
  assert!(matches!(exit, Exit::Success(None)), "{exit:?}");
  assert_eq!(took, Duration::from_secs(2));
  assert!(client.recorded_queries().is_empty());
}

#[test]
fn a_test_random_picks_each_calls_fault() {
  let random = TestRandom::seeded(0);
  random.feed_f64s([0.9, 0.1, 0.6]);
  let plan: FaultPlan<SqlError> = FaultPlan::from_random(random.random())
    .with(0.5, Fault::timeout())
    .with(0.25, Fault::delay(Duration::from_secs(1)));
  let kinds: Vec<_> = (0..3)
    .map(|_| plan.next_fault().map(|f| f.kind()))
    .collect();
  assert_eq!(
    kinds,
    [
      None,
      Some(FaultKind::Timeout),
      Some(FaultKind::Delay(Duration::from_secs(1)))
    ]
  );
  assert_eq!((plan.seed(), plan.calls()), (None, 3));
}

#[test]
#[should_panic(expected = "add up to")]
fn rules_over_probability_one_panic() {
  let _ = FaultPlan::<SqlError>::seeded(0)
    .with(0.6, Fault::timeout())
    .with(0.5, Fault::timeout());
}

#[test]
fn delay_queries_on_the_sql_double_is_latency_too() {
  let client = sql_double();
  client.delay_queries(Duration::from_millis(1500));
  let (exit, took) = run_virtual(client.query(Q, &[]).timeout(Duration::from_secs(1)));
  assert_eq!(
    (exit.into_result().ok(), took),
    (Some(None), Duration::from_secs(1))
  );
}

#[cfg(feature = "http")]
#[test]
fn faults_apply_to_the_http_test_client() {
  use axum::routing::get;

  use crate::http::{HttpTestError, StatusCode, TestClient};

  let client = TestClient::new(axum::Router::new().route("/health", get(|| async { "ok" })));
  let plan = FaultPlan::seeded(5).with(1.0, Fault::delay(Duration::from_secs(2)));
  let (exit, took) = run_virtual(plan.inject(client.get("/health").send()));
  let response = exit.into_result().expect("a response");
  assert_eq!(
    (response.status(), took),
    (StatusCode::OK, Duration::from_secs(2))
  );

  let injected = HttpTestError::Service("injected".to_owned());
  let (exit, _) = run_virtual(Fault::fail(injected.clone()).apply(client.get("/health").send()));
  assert!(matches!(exit, Exit::Failure(Cause::Fail(e)) if e == injected));
}
