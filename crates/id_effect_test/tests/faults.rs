//! Fault injection end to end (feature `fault`): a retry + deadline policy over the `SqlClient`
//! double, under seeded plans of latency, failures and hangs, on the `TestExecutor` virtual clock.
//!
//! - Each attempt draws its fault when it starts; the policy's outcome and its virtual time are
//!   exactly what the plan's history says, for every seed of a sweep.
//! - A seed whose schedule beats the policy is reported with `FaultPlan::seed` and replays to the
//!   same history and the same failure.
//! - `TestSqlClient::delay_queries` and a plan's `Fault::delay` add up against one deadline.

#![cfg(feature = "fault")]

use std::collections::HashSet;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Executor, Exit, Schedule, TestExecutor, fail, succeed};
use id_effect_sql::{SqlClient, SqlError, SqlRow, TestSqlClient};
use id_effect_test::{Fault, FaultKind, FaultPlan};

const Q: &str = "SELECT balance FROM accounts WHERE id = 1";
/// The caller's deadline on each attempt.
const DEADLINE: Duration = Duration::from_secs(1);
/// The retry schedule's first backoff (doubled after each retry).
const BACKOFF: Duration = Duration::from_millis(100);
/// Latency under the deadline: the attempt answers late.
const SLOW: Duration = Duration::from_millis(250);
/// Latency past the deadline: the attempt is abandoned before it runs.
const STALLED: Duration = Duration::from_secs(5);

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
fn accounts_db() -> TestSqlClient {
  let client = TestSqlClient::new();
  client.script_query(Q, vec![SqlRow::from_pairs([("balance", 120i64)])]);
  assert!(client.connect().block_on().into_result().is_ok());
  client
}

fn injected() -> SqlError {
  SqlError::connection_failed("injected")
}

fn deadline_passed() -> SqlError {
  SqlError::connection_failed("deadline passed")
}

/// Latency (under and past the deadline), failures and hangs: 65 % of the calls are faulted.
fn chaos(seed: u64) -> FaultPlan<SqlError> {
  FaultPlan::seeded(seed)
    .with(0.2, Fault::delay(SLOW))
    .with(0.2, Fault::delay(STALLED))
    .with(0.15, Fault::fail(injected()))
    .with(0.1, Fault::timeout())
}

/// The code under test: one balance read, each attempt bounded by `DEADLINE`, retried with
/// exponential backoff up to `retries` times. The plan is injected into every attempt.
fn read_balance(
  client: &TestSqlClient,
  plan: &FaultPlan<SqlError>,
  retries: u64,
) -> Effect<i64, SqlError> {
  let (client, plan) = (client.clone(), plan.clone());
  let attempt = move || {
    plan
      .inject(client.query(Q, &[]))
      .timeout_fail(DEADLINE, deadline_passed)
      .flat_map(|rows| match rows[0].get::<i64>("balance") {
        Ok(balance) => succeed(balance),
        Err(e) => fail(e),
      })
  };
  Effect::retry(
    attempt,
    Schedule::exponential(BACKOFF).intersect(Schedule::recurs(retries)),
  )
}

/// What an attempt with this fault does to the policy: whether it succeeds, the virtual time it
/// takes, and the error it fails with.
fn attempt_outcome(fault: Option<FaultKind>) -> (bool, Duration, Option<SqlError>) {
  match fault {
    None => (true, Duration::ZERO, None),
    Some(FaultKind::Delay(d)) if d < DEADLINE => (true, d, None),
    Some(FaultKind::Delay(_)) | Some(FaultKind::Timeout) => {
      (false, DEADLINE, Some(deadline_passed()))
    }
    Some(FaultKind::Fail) => (false, Duration::ZERO, Some(injected())),
  }
}

/// The exit and virtual time the policy must have given, from the plan's history alone.
fn expected_from(history: &[Option<FaultKind>]) -> (Result<i64, SqlError>, Duration) {
  let mut took = Duration::ZERO;
  let mut last_error = None;
  for (i, fault) in history.iter().enumerate() {
    if i > 0 {
      took += BACKOFF * 2u32.pow(i as u32 - 1);
    }
    let (ok, cost, error) = attempt_outcome(*fault);
    took += cost;
    if ok {
      assert_eq!(i, history.len() - 1, "no attempt after a success");
      return (Ok(120), took);
    }
    last_error = error;
  }
  (Err(last_error.expect("at least one attempt")), took)
}

fn simplify(exit: Exit<i64, SqlError>) -> Result<i64, SqlError> {
  match exit {
    Exit::Success(balance) => Ok(balance),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => panic!("not a typed failure: {cause:?}"),
  }
}

#[test]
fn retry_and_deadline_survive_seeded_chaos() {
  let wall = Instant::now();
  let (mut survived, mut retried, mut virtual_time) = (0, 0, Duration::ZERO);
  let mut drawn = HashSet::new();
  for seed in 0..64 {
    let client = accounts_db();
    let plan = chaos(seed);
    let (exit, took) = run_virtual(read_balance(&client, &plan, 6));
    let history = plan.history();
    assert!(
      (1..=7).contains(&history.len()),
      "seed {seed}: one draw per attempt"
    );
    let (expected, expected_took) = expected_from(&history);
    let got = simplify(exit);
    assert_eq!(
      (&got, took),
      (&expected, expected_took),
      "seed {seed}: {history:?}"
    );
    // Only the attempt that answered ran: failed and hung attempts never reach the database,
    // and an attempt delayed past its deadline is abandoned before it runs.
    assert_eq!(
      client.recorded_queries().len(),
      usize::from(got.is_ok()),
      "seed {seed}"
    );
    survived += usize::from(got.is_ok());
    retried += usize::from(history.len() > 1);
    virtual_time += took;
    drawn.extend(history);
  }
  // 45 % of the attempts lose (stalled, failed, hung), so about 0.45^7 of the seeds exhaust 6
  // retries: the policy carries all but a few.
  assert!(survived >= 60, "{survived}/64 seeds survived");
  assert!(retried > 0, "some seed needed a retry");
  for kind in [
    None,
    Some(FaultKind::Delay(SLOW)),
    Some(FaultKind::Delay(STALLED)),
    Some(FaultKind::Fail),
    Some(FaultKind::Timeout),
  ] {
    assert!(drawn.contains(&kind), "{kind:?} drawn in the sweep");
  }
  assert!(
    wall.elapsed() < Duration::from_secs(10),
    "{virtual_time:?} of virtual time took {:?} of wall time",
    wall.elapsed()
  );
}

#[test]
fn a_seed_that_beats_the_policy_replays() {
  // Two retries are not enough for every schedule: find the first seed that wins.
  let (seed, history, exit) = (0..)
    .find_map(|seed| {
      let plan = chaos(seed);
      let (exit, _) = run_virtual(read_balance(&accounts_db(), &plan, 2));
      let exit = simplify(exit);
      exit
        .is_err()
        .then(|| (plan.seed().expect("a seeded plan"), plan.history(), exit))
    })
    .expect("some seed beats two retries");
  assert_eq!(history.len(), 3, "every attempt was drawn and lost");

  // The reported seed replays the same schedule to the same failure.
  let plan = chaos(seed);
  let (again, _) = run_virtual(read_balance(&accounts_db(), &plan, 2));
  assert_eq!((plan.history(), simplify(again)), (history.clone(), exit));
  // A policy with more retries, tested against that very schedule, outlasts it: its first three
  // draws are the same.
  let plan = chaos(seed);
  let (exit, _) = run_virtual(read_balance(&accounts_db(), &plan, 20));
  assert_eq!(simplify(exit), Ok(120), "seed {seed}");
  assert_eq!(&plan.history()[..3], &history[..]);
  assert!(plan.history().len() > 3);
}

#[test]
fn double_latency_and_plan_latency_add_up() {
  let client = accounts_db();
  client.delay_queries(Duration::from_millis(400));
  let plan = FaultPlan::seeded(9).with(1.0, Fault::delay(Duration::from_millis(700)));

  // 700 ms (plan) + 400 ms (double) is past a 1 s deadline...
  let (exit, took) = run_virtual(
    plan
      .inject(client.query(Q, &[]))
      .timeout_fail(DEADLINE, deadline_passed),
  );
  assert!(
    matches!(exit, Exit::Failure(Cause::Fail(ref e)) if *e == deadline_passed()),
    "{exit:?}"
  );
  assert_eq!(took, DEADLINE);
  assert!(client.recorded_queries().is_empty(), "abandoned in flight");

  // ...and within a 2 s one, which waits for both.
  let (exit, took) = run_virtual(
    plan
      .inject(client.query(Q, &[]))
      .timeout_fail(Duration::from_secs(2), deadline_passed),
  );
  assert_eq!(exit.into_result().map(|rows| rows.len()).ok(), Some(1));
  assert_eq!(took, Duration::from_millis(1100));
  assert_eq!(
    plan.history(),
    [Some(FaultKind::Delay(Duration::from_millis(700))); 2]
  );
}
