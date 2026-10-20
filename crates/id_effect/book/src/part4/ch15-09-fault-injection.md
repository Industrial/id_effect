# Fault Injection

Retries, deadlines, fallbacks and supervisors are code paths like any other, but they only run
when something goes wrong — so a test has to make it go wrong, on purpose and the same way each
time. `id_effect` gives a test these faults, all deterministic:

| Fault | Where | What it does |
|-------|-------|--------------|
| Latency | `Fault::delay`, `TestSqlClient::delay_queries` | the call starts later, on the executor's clock (virtual under `TestExecutor`) |
| Failures and hangs | `Fault::fail`, `Fault::timeout`; `TestSqlClient::fail_queries`, `fail_commits` | the call fails without running, or never answers |
| Seeded schedules | `FaultPlan::seeded(seed).with(p, fault)` | which call gets which fault, reproducible from the seed |
| Partitions | `MemoryNetwork::partition`, `heal` (`id_effect_node`) | the link between two nodes dies, and comes back |
| Crashes | `ProcessNode::exit(pid, ExitReason::Killed)` (`id_effect_process`) | the process dies, whatever it traps |

`Fault` and `FaultPlan` are `id_effect_test`'s `fault` feature; they work on any `Effect` — a
service double's calls, a real client's, your own:

```toml
[dev-dependencies]
id_effect_test = { version = "0.12", features = ["fault"] }
```

The snippets below run their effects with `run_virtual`, the helper from
[Virtual Time with TestExecutor](./ch15-02-test-clock.md): it runs an effect on a fresh
`TestExecutor`, advancing the clock from timer to timer, and returns the `Exit` with the virtual
time it took.

## Latency

`Fault::delay(d)` makes a call start `d` later: a `sleep` on the fiber's executor clock, then the
call as it would have run. On a `TestExecutor` the sleep is virtual, so thirty seconds of latency
cost no wall time, and the call has not run until the clock gets there:

```rust
use std::sync::Arc;
use std::time::Duration;

use id_effect::TestExecutor;
use id_effect_sql::{SqlClient, SqlError, SqlRow, TestSqlClient};
use id_effect_test::Fault;

let db = TestSqlClient::new();
db.script_query("SELECT 1", vec![SqlRow::from_pairs([("one", 1i64)])]);
db.connect().block_on().into_result().unwrap();

let slow = Fault::<SqlError>::delay(Duration::from_secs(30));
let exec = TestExecutor::new();
let h = slow.apply(db.query("SELECT 1", &[])).fork_root(Arc::new(exec.clone()));

exec.run_until_stalled();
exec.advance(Duration::from_secs(29));
assert!(!h.is_done());
assert!(db.recorded_queries().is_empty()); // not sent yet
exec.advance(Duration::from_secs(1));
assert!(h.is_done());
assert_eq!(db.recorded_queries().len(), 1);
```

The SQL double has latency of its own: `TestSqlClient::delay_queries(d)` delays every `query`
(`Duration::ZERO` removes it). It is the quickest way to put a whole suite behind a slow database;
a `Fault` is for one call, or — through a plan — some of them. The two add up:

```rust
# use std::future::Future;
# use std::pin::pin;
# use std::sync::Arc;
# use std::task::{Context, Poll, Waker};
# use std::time::Duration;
# use id_effect::{Effect, Executor, Exit, TestExecutor};
# fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
#   let exec = TestExecutor::new();
#   let start = exec.now();
#   let h = eff.fork_root(Arc::new(exec.clone()));
#   exec.run_until_stalled();
#   while !h.is_done() {
#     let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
#     exec.advance(deadline.saturating_duration_since(exec.now()));
#   }
#   let exit = match pin!(h.join_future()).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
#     Poll::Ready(exit) => exit,
#     Poll::Pending => unreachable!(),
#   };
#   (exit, exec.now() - start)
# }
use id_effect_sql::{SqlClient, SqlError, SqlRow, TestSqlClient};
use id_effect_test::Fault;

let db = TestSqlClient::new();
db.script_query("SELECT 1", vec![SqlRow::from_pairs([("one", 1i64)])]);
db.connect().block_on().into_result().unwrap();
db.delay_queries(Duration::from_millis(400));

let call = Fault::<SqlError>::delay(Duration::from_millis(700)).apply(db.query("SELECT 1", &[]));
let (exit, took) = run_virtual(call.timeout(Duration::from_secs(1)));
assert!(matches!(exit, Exit::Success(None))); // 1.1 s is past the 1 s deadline
assert_eq!(took, Duration::from_secs(1));
```

## Failures and hangs

`Fault::fail(e)` fails the call with `e` without running it — a refused connection, a dropped
query. `Fault::timeout()` is the dependency that accepted the request and went silent: the call
never answers and never runs, and only the caller's own deadline (`timeout`, `timeout_fail`) or an
interrupt ends it. A test with a hang and no deadline waits forever — which is the bug the fault
is there to find.

```rust
# use std::future::Future;
# use std::pin::pin;
# use std::sync::Arc;
# use std::task::{Context, Poll, Waker};
# use std::time::Duration;
# use id_effect::{Effect, Executor, Exit, TestExecutor};
# fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
#   let exec = TestExecutor::new();
#   let start = exec.now();
#   let h = eff.fork_root(Arc::new(exec.clone()));
#   exec.run_until_stalled();
#   while !h.is_done() {
#     let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
#     exec.advance(deadline.saturating_duration_since(exec.now()));
#   }
#   let exit = match pin!(h.join_future()).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
#     Poll::Ready(exit) => exit,
#     Poll::Pending => unreachable!(),
#   };
#   (exit, exec.now() - start)
# }
use id_effect::Cause;
use id_effect_sql::{SqlClient, SqlError, TestSqlClient};
use id_effect_test::Fault;

let db = TestSqlClient::new();
db.connect().block_on().into_result().unwrap();

let refused = SqlError::connection_failed("refused");
let (exit, took) = run_virtual(Fault::fail(refused.clone()).apply(db.query("SELECT 1", &[])));
assert!(matches!(exit, Exit::Failure(Cause::Fail(e)) if e == refused));
assert_eq!(took, Duration::ZERO);

let deadline = || SqlError::connection_failed("deadline passed");
let silent = Fault::timeout().apply(db.query("SELECT 1", &[]));
let (exit, took) = run_virtual(silent.timeout_fail(Duration::from_secs(2), deadline));
assert!(matches!(exit, Exit::Failure(Cause::Fail(e)) if e == deadline()));
assert_eq!(took, Duration::from_secs(2));
assert!(db.recorded_queries().is_empty()); // neither call reached the database
```

For a double that should fail every time, its own switches are shorter: `fail_queries(message)`,
`fail_commits()`, `fail_on_connect()` and `fail_commits_with_serialization_failure(n)` on
`TestSqlClient`; a handler that returns the error on the HTTP `TestClient`'s router.

## Seeded fault schedules

One fault on one call tests one path. Real outages are a mix — most calls fine, some slow, a few
failing — and the interesting bugs are in the sequences. A `FaultPlan` decides each call's fault:

- `FaultPlan::seeded(seed)` starts a plan with no rules; `.with(probability, fault)` adds one. The
  rules are exclusive (a call gets at most one fault), so their probabilities add up to at most 1;
  the rest is "no fault".
- `plan.inject(call)` wraps a call: when it **starts**, the plan draws one number from its seeded
  generator, picks the fault (or none) and applies it.
- `plan.history()` lists each decided call's `FaultKind` (or `None`) in the order the calls
  started; `plan.seed()` is the seed to report with a failure; `plan.calls()` counts them.

One draw per call, whatever the outcome, so the schedule depends only on the seed, the rules and
how many calls came before: the same seed and the same calls give the same faults, run after run.

```rust
# use std::future::Future;
# use std::pin::pin;
# use std::sync::Arc;
# use std::task::{Context, Poll, Waker};
# use id_effect::{Effect, Executor, Exit, TestExecutor};
# fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
#   let exec = TestExecutor::new();
#   let start = exec.now();
#   let h = eff.fork_root(Arc::new(exec.clone()));
#   exec.run_until_stalled();
#   while !h.is_done() {
#     let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
#     exec.advance(deadline.saturating_duration_since(exec.now()));
#   }
#   let exit = match pin!(h.join_future()).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
#     Poll::Ready(exit) => exit,
#     Poll::Pending => unreachable!(),
#   };
#   (exit, exec.now() - start)
# }
use std::time::Duration;

use id_effect_sql::{SqlClient, SqlError, SqlRow, TestSqlClient};
use id_effect_test::{Fault, FaultKind, FaultPlan};

fn chaos(seed: u64) -> FaultPlan<SqlError> {
  FaultPlan::seeded(seed)
    .with(0.3, Fault::delay(Duration::from_millis(250)))
    .with(0.2, Fault::fail(SqlError::connection_failed("injected")))
    .with(0.1, Fault::timeout())
}

/// 32 calls under `chaos(seed)`, each bounded by 1 s: the schedule and the virtual time.
fn run(seed: u64) -> (Vec<Option<FaultKind>>, Duration) {
  let db = TestSqlClient::new();
  db.script_query("SELECT 1", vec![SqlRow::from_pairs([("one", 1i64)])]);
  db.connect().block_on().into_result().unwrap();
  let plan = chaos(seed);
  let took = (0..32)
    .map(|_| run_virtual(plan.inject(db.query("SELECT 1", &[])).timeout(Duration::from_secs(1))).1)
    .sum();
  (plan.history(), took)
}

let (schedule, took) = run(7);
assert_eq!(schedule.len(), 32); // one decision per call
assert!(schedule.contains(&Some(FaultKind::Timeout)));
assert_eq!(run(7), (schedule.clone(), took)); // the same seed, the same faults, the same time
assert_ne!(run(8).0, schedule); // another seed, another schedule
```

Calls decide in the order they **start**. Sequential calls, or concurrent ones on the
single-threaded `TestExecutor`, start in a deterministic order; on a multi-threaded runtime the
order — and so the schedule — can change between runs.

A plan is cheap to clone, and clones share the generator and the history: move one into a retry
factory or a hand-written double and keep another to inspect afterwards. To pick each call's
fault exactly instead of by seed, build the plan with `FaultPlan::from_random` over a
`TestRandom` whose queued `f64`s are the draws:

```rust
use std::time::Duration;

use id_effect::TestRandom;
use id_effect_sql::SqlError;
use id_effect_test::{Fault, FaultKind, FaultPlan};

let random = TestRandom::seeded(0);
random.feed_f64s([0.9, 0.1, 0.6]); // rule ranges: [0, 0.5) hang, [0.5, 0.75) delay, rest none
let plan: FaultPlan<SqlError> = FaultPlan::from_random(random.random())
  .with(0.5, Fault::timeout())
  .with(0.25, Fault::delay(Duration::from_secs(1)));
let kinds: Vec<_> = (0..3).map(|_| plan.next_fault().map(|f| f.kind())).collect();
assert_eq!(kinds, [None, Some(FaultKind::Timeout), Some(FaultKind::Delay(Duration::from_secs(1)))]);
```

## Testing a resilience policy

The point of all this is a policy under test. Here it is a balance read with a 1 s deadline per
attempt and exponential retries (see [retry and repeat](../part3/ch11-03-retry-repeat.md)).
The plan is injected **inside** the retry's factory, so every attempt draws its own fault. A sweep
over seeds finds the schedules the policy loses; each one replays from its seed, and the fix is
checked against that very schedule:

```rust
# use std::future::Future;
# use std::pin::pin;
# use std::sync::Arc;
# use std::task::{Context, Poll, Waker};
# use id_effect::{Executor, Exit, TestExecutor};
# fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (Exit<A, E>, Duration) {
#   let exec = TestExecutor::new();
#   let start = exec.now();
#   let h = eff.fork_root(Arc::new(exec.clone()));
#   exec.run_until_stalled();
#   while !h.is_done() {
#     let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
#     exec.advance(deadline.saturating_duration_since(exec.now()));
#   }
#   let exit = match pin!(h.join_future()).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
#     Poll::Ready(exit) => exit,
#     Poll::Pending => unreachable!(),
#   };
#   (exit, exec.now() - start)
# }
use std::time::Duration;

use id_effect::{Effect, Schedule, fail, succeed};
use id_effect_sql::{SqlClient, SqlError, SqlRow, TestSqlClient};
use id_effect_test::{Fault, FaultKind, FaultPlan};

const Q: &str = "SELECT balance FROM accounts WHERE id = 1";

fn accounts_db() -> TestSqlClient {
  let db = TestSqlClient::new();
  db.script_query(Q, vec![SqlRow::from_pairs([("balance", 120i64)])]);
  db.connect().block_on().into_result().unwrap();
  db
}

/// Latency in time (250 ms) and past the deadline (5 s), failures, hangs.
fn chaos(seed: u64) -> FaultPlan<SqlError> {
  FaultPlan::seeded(seed)
    .with(0.2, Fault::delay(Duration::from_millis(250)))
    .with(0.2, Fault::delay(Duration::from_secs(5)))
    .with(0.15, Fault::fail(SqlError::connection_failed("injected")))
    .with(0.1, Fault::timeout())
}

/// The policy under test.
fn read_balance(db: TestSqlClient, plan: FaultPlan<SqlError>, retries: u64) -> Effect<i64, SqlError> {
  let attempt = move || {
    plan
      .inject(db.query(Q, &[]))
      .timeout_fail(Duration::from_secs(1), || SqlError::connection_failed("deadline passed"))
      .flat_map(|rows| match rows[0].get::<i64>("balance") {
        Ok(balance) => succeed(balance),
        Err(e) => fail(e),
      })
  };
  Effect::retry(
    attempt,
    Schedule::exponential(Duration::from_millis(100)).intersect(Schedule::recurs(retries)),
  )
}

// The sweep: which seeds beat three retries?
let lost: Vec<(u64, Vec<Option<FaultKind>>)> = (0..32)
  .filter_map(|seed| {
    let plan = chaos(seed);
    let (exit, _) = run_virtual(read_balance(accounts_db(), plan.clone(), 3));
    exit.into_result().is_err().then(|| (plan.seed().unwrap(), plan.history()))
  })
  .collect();
assert!(!lost.is_empty(), "this plan beats three retries now and then");

for (seed, schedule) in lost {
  assert_eq!(schedule.len(), 4); // all four attempts drawn, all four lost
  // Replay: the same seed loses the same way.
  let plan = chaos(seed);
  let (exit, _) = run_virtual(read_balance(accounts_db(), plan.clone(), 3));
  assert!(exit.into_result().is_err());
  assert_eq!(plan.history(), schedule);
  // The fix, against that very schedule: more retries outlast it.
  let plan = chaos(seed);
  let (exit, took) = run_virtual(read_balance(accounts_db(), plan.clone(), 8));
  assert!(matches!(exit, Exit::Success(120)), "seed {seed}");
  assert_eq!(plan.history()[..4], schedule[..]);
  assert!(took > Duration::from_secs(3)); // minutes of backoff would cost no wall time either
}
```

Because every delay is virtual, the virtual time is exact too: an attempt lost to the deadline
costs 1 s, a refused one nothing, and the backoff before retry `i` is `100 ms × 2^i` — so a test
can assert the time a schedule takes, not just its outcome (`crates/id_effect_test/tests/faults.rs`
does, for every seed of a sweep).

The same goes for the rest of the resilience toolbox: inject the plan into the call that an
`id_effect_resilience` guard (`CircuitBreaker`, `Bulkhead`, `Fallback`) wraps, and the plan's
history is exactly what the guard saw.

## Partitions

Between nodes of a cluster the fault is the network. `id_effect_node`'s `MemoryNetwork` is an
in-process network for tests: each simulated node gets a transport with its own address,
`partition(a, b)` cuts the link between two nodes (both ways), `heal(a, b)` restores it. Like TCP,
a partition kills the connections that existed — they stay dead after the heal — and dials fail
until it heals:

```rust
use id_effect_node::transport::Transport;
use id_effect_node::transport::memory::MemoryNetwork;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let net = MemoryNetwork::new();
let (a, b) = (net.transport("a"), net.transport("b"));
let _listening = (a.listen("a").await.unwrap(), b.listen("b").await.unwrap());

let before = a.connect("b").await.unwrap();
net.partition("a", "b");
assert!(a.connect("b").await.is_err()); // new dials fail
assert!(before.send(b"ping".to_vec().into()).await.is_err()); // and old links are dead

net.heal("a", "b");
assert!(a.connect("b").await.is_ok()); // only new dials succeed
assert!(before.send(b"ping".to_vec().into()).await.is_err());
# }
```

Partitions are imposed and healed synchronously, so a netsplit test is a script: cut, drive the
cluster, assert, heal. Membership (`id_effect_node::membership`) runs on a virtual timer wheel for
the same reason; `crates/id_effect_node/tests/chaos_partition.rs` runs asymmetric partitions and
flapping links on it with no sleeps and no real clock.

## Crashes

A process (see [Processes and Supervision Trees](../part7/ch37-00-processes-supervision.md)) is
killed with `ProcessNode::exit(pid, ExitReason::Killed)`, Erlang's `exit(Pid, kill)`: untrappable,
so the process dies even when it traps exits. Its links and monitors fire, and its supervisor
restarts it under the strategy and intensity it was given — the crash a supervision test needs:

```rust
# use std::future::Future;
# use std::pin::pin;
# use std::task::{Context, Poll, Waker};
use std::sync::Arc;

use id_effect::{Effect, Exit, Never, TestExecutor, succeed};
use id_effect_process::{ExitReason, Next, NodeName, Process, ProcessCtx, ProcessNode};

struct Worker;

impl Process for Worker {
  type Args = ();
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = Never;

  fn init(_: (), _: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Worker)
  }

  fn handle_call(self, _: (), _: ProcessCtx) -> Effect<((), Next<Self>), Never, ()> {
    succeed(((), Next::Continue(self)))
  }
}

let exec = TestExecutor::new();
let node = ProcessNode::on_executor(NodeName::new("t@test"), Arc::new(exec.clone()));
let worker = node.spawn::<Worker>((), ());
exec.run_until_stalled();
assert!(worker.is_alive());

node.exit(worker.pid(), ExitReason::Killed);
let exited: Effect<ExitReason, Never> = node.await_exit(worker.pid());
let h = exited.fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
# let exit = match pin!(h.join_future()).as_mut().poll(&mut Context::from_waker(Waker::noop())) {
#   Poll::Ready(exit) => exit,
#   Poll::Pending => panic!("the process is not finalized"),
# };
assert!(matches!(exit, Exit::Success(ExitReason::Killed)));
assert!(!worker.is_alive());
```

Other abnormal reasons (`ExitReason::Error(..)`) are trappable: a process that traps exits gets
them as a message instead of dying. `crates/id_effect_process/tests/supervision_contracts.rs`
crashes children under each restart strategy (one-for-one, one-for-all, rest-for-one) on a
`TestExecutor` and checks which ones come back with a fresh pid.

## What is not here yet

Faults are injected by the test, around calls it controls. There is no switch to turn faults on
in a running application, and no network proxy (toxiproxy-style) between a service and a real
database; both are planned with the distributed-systems work in 0.11.

**Runnable example:** `cargo run -p id_effect_test --example 030_faults --features fault` sweeps
seeds over the policy above and prints each schedule; pass a seed to replay it. The integration
test is `crates/id_effect_test/tests/faults.rs`.
