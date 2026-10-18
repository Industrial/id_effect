//! Ex 001 — CQRS over an in-memory store, and optimistic concurrency with `append_expected`.
//!
//! ```bash
//! cargo run -p id_effect_events --example 001_expected_version
//! ```

use id_effect::{Cause, Effect, Exit, fail, succeed};
use id_effect_events::{
  CommandHandler, EventStore, EventStoreError, MemoryEventStore, Projection, QueryHandler,
  dispatch_command, query_projection,
};
use id_effect_tokio::TokioRuntime;

#[derive(Clone, Debug, PartialEq)]
enum Event {
  Deposited(i64),
  Withdrawn(i64),
}

enum Command {
  Deposit(i64),
  Withdraw(i64),
}

#[derive(Debug)]
struct Rejected(&'static str);
impl std::fmt::Display for Rejected {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.0)
  }
}
impl std::error::Error for Rejected {}

/// Write side: validate a command, emit events.
struct Accounts;
impl CommandHandler<Command, Event> for Accounts {
  type Error = Rejected;
  fn handle(&self, command: Command) -> Effect<Vec<Event>, Rejected> {
    match command {
      Command::Deposit(n) if n > 0 => succeed(vec![Event::Deposited(n)]),
      Command::Withdraw(n) if n > 0 => succeed(vec![Event::Withdrawn(n)]),
      _ => fail(Rejected("amounts must be positive")),
    }
  }
}

/// Read model: the balance folded from the stream.
struct Balance;
impl Projection<i64, Event> for Balance {
  fn initial(&self) -> i64 {
    0
  }
  fn apply(&self, balance: i64, event: &Event) -> i64 {
    match event {
      Event::Deposited(n) => balance + n,
      Event::Withdrawn(n) => balance - n,
    }
  }
}

/// Read side: answer a question (here, a fixed overdraft limit).
struct OverdraftLimit;
impl QueryHandler<(), i64> for OverdraftLimit {
  type Error = std::convert::Infallible;
  fn query(&self, (): ()) -> Effect<i64, Self::Error> {
    succeed(100)
  }
}

async fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(eff: Effect<A, E>) -> A {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => a,
    Exit::Failure(cause) => panic!("example failed: {cause:?}"),
  }
}

#[tokio::main]
async fn main() {
  let store = MemoryEventStore::new();

  let balance = run(dispatch_command(
    &Accounts,
    &store,
    "account-1",
    Balance,
    Command::Deposit(50),
  ))
  .await;
  let balance_after = run(dispatch_command(
    &Accounts,
    &store,
    "account-1",
    Balance,
    Command::Withdraw(20),
  ))
  .await;
  println!("balance: {balance} then {balance_after}");
  assert_eq!((balance, balance_after), (50, 30));

  let limit = run(query_projection(
    &OverdraftLimit,
    &store,
    "account-1",
    Balance,
    (),
  ))
  .await;
  println!("overdraft limit: {limit}");

  // Two writers both read the stream at version 2; the first append wins, the second is stale.
  let version = run(store.latest_version("account-1")).await;
  run(store.append_expected("account-1", version, &[Event::Deposited(5)])).await;
  let stale = TokioRuntime::current()
    .expect("tokio")
    .run(store.append_expected("account-1", version, &[Event::Withdrawn(5)]))
    .await;
  match stale {
    Exit::Failure(Cause::Fail(EventStoreError::VersionConflict {
      expected, actual, ..
    })) => println!("stale writer: expected version {expected}, stream is at {actual}"),
    other => panic!("expected a version conflict, got {other:?}"),
  }
}
