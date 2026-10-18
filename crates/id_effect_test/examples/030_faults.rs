//! Ex 030 — fault injection: a balance read with a 1 s deadline per attempt and exponential
//! retries, run over the `SqlClient` double under a seeded `FaultPlan` of latency, failures and
//! hangs, on the `TestExecutor` virtual clock. Each seed prints its schedule (one fault draw per
//! attempt), the outcome and the virtual time it took; the whole sweep takes no wall time.
//!
//! A seed that beats the policy prints `LOST`; pass it to replay that schedule alone:
//! `cargo run -p id_effect_test --example 030_faults --features fault -- <seed>`. See the book
//! chapter "Fault Injection".
//!
//! Run: `cargo run -p id_effect_test --example 030_faults --features fault`

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::{Effect, Executor, Exit, Schedule, TestExecutor, fail, succeed};
use id_effect_sql::{SqlClient, SqlError, SqlRow, TestSqlClient};
use id_effect_test::{Fault, FaultKind, FaultPlan};

const Q: &str = "SELECT balance FROM accounts WHERE id = 1";

/// Runs `eff` on a fresh `TestExecutor`, advancing from timer to timer; its exit and the virtual
/// time it took.
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
  let exit = match pin!(h.join_future())
    .as_mut()
    .poll(&mut Context::from_waker(Waker::noop()))
  {
    Poll::Ready(exit) => exit,
    Poll::Pending => unreachable!("the fiber is done"),
  };
  (exit, exec.now() - start)
}

/// 35 % of the calls are slow but in time, 45 % lose: stalled past the deadline, failed or hung.
fn chaos(seed: u64) -> FaultPlan<SqlError> {
  FaultPlan::seeded(seed)
    .with(0.2, Fault::delay(Duration::from_millis(250)))
    .with(0.2, Fault::delay(Duration::from_secs(5)))
    .with(0.15, Fault::fail(SqlError::connection_failed("injected")))
    .with(0.1, Fault::timeout())
}

/// The policy under test: each attempt bounded by 1 s, three retries backing off from 100 ms.
fn read_balance(client: TestSqlClient, plan: FaultPlan<SqlError>) -> Effect<i64, SqlError> {
  let attempt = move || {
    plan
      .inject(client.query(Q, &[]))
      .timeout_fail(Duration::from_secs(1), || {
        SqlError::connection_failed("deadline passed")
      })
      .flat_map(|rows| match rows[0].get::<i64>("balance") {
        Ok(balance) => succeed(balance),
        Err(e) => fail(e),
      })
  };
  Effect::retry(
    attempt,
    Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(3)),
  )
}

fn show(fault: &Option<FaultKind>) -> String {
  match fault {
    None => "ok".to_owned(),
    Some(FaultKind::Delay(d)) => format!("+{d:?}"),
    Some(FaultKind::Fail) => "fail".to_owned(),
    Some(FaultKind::Timeout) => "hang".to_owned(),
  }
}

fn main() {
  let seeds: Vec<u64> = match std::env::args().nth(1) {
    Some(seed) => vec![seed.parse().expect("a seed (u64)")],
    None => (0..32).collect(),
  };
  let wall = Instant::now();
  let mut lost = Vec::new();
  for seed in seeds {
    let client = TestSqlClient::new();
    client.script_query(Q, vec![SqlRow::from_pairs([("balance", 120i64)])]);
    assert!(client.connect().block_on().into_result().is_ok());
    let plan = chaos(seed);

    let (exit, took) = run_virtual(read_balance(client, plan.clone()));
    let schedule: Vec<String> = plan.history().iter().map(show).collect();
    let outcome = match exit.into_result() {
      Ok(balance) => format!("balance {balance}"),
      Err(cause) => {
        lost.push(seed);
        format!("LOST {cause:?}")
      }
    };
    println!(
      "seed {seed:>2}: [{}] -> {outcome} after {took:?} (virtual)",
      schedule.join(", ")
    );
  }
  println!(
    "lost: {lost:?}; wall time {:?} for the whole sweep",
    wall.elapsed()
  );
}
