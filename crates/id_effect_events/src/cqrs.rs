//! CQRS boundary types — command handlers produce events; query handlers read projections.
//!
//! [`dispatch_command`] and [`query_projection`] return `Effect`s: nothing is handled, appended
//! or read until the caller runs them.

use crate::error::EventStoreError;
use crate::event_store::EventStore;
use crate::projection::{Projection, run_projection_from_store};
use id_effect::Effect;

/// Turn a command into zero or more domain events (write side).
pub trait CommandHandler<Cmd, Ev> {
  /// Error type for rejected commands.
  type Error: std::error::Error + Send + Sync + 'static;
  /// Validate and map `command` to events without persisting.
  fn handle(&self, command: Cmd) -> Effect<Vec<Ev>, Self::Error>;
}

/// Answer a query from a materialized read model (read side).
pub trait QueryHandler<Qry, Ans> {
  /// Error type for failed queries.
  type Error: std::error::Error + Send + Sync + 'static;
  /// Run the query.
  fn query(&self, query: Qry) -> Effect<Ans, Self::Error>;
}

/// Handle `command`, append its events to `stream_id`, then fold the stream with `projection`
/// into the updated read model.
///
/// The effect owns a clone of `store` (stores are cheap handles: [`MemoryEventStore`] and
/// [`SqlEventJournal`] share their state; wrap others in an `Arc`).
///
/// [`MemoryEventStore`]: crate::MemoryEventStore
/// [`SqlEventJournal`]: crate::SqlEventJournal
pub fn dispatch_command<Cmd, Ev, S, P, H, Store>(
  handler: &H,
  store: &Store,
  stream_id: &str,
  projection: P,
  command: Cmd,
) -> Effect<S, DispatchError<H::Error>>
where
  H: CommandHandler<Cmd, Ev>,
  Store: EventStore<Ev> + Clone + 'static,
  P: Projection<S, Ev> + Send + 'static,
  Ev: Clone + Send + Sync + 'static,
  S: Send + 'static,
{
  let store = store.clone();
  let stream_id = stream_id.to_owned();
  handler
    .handle(command)
    .map_err(DispatchError::Command)
    .flat_map(move |events| {
      let project = run_projection_from_store(&store, &stream_id, 1, projection);
      store
        .append(&stream_id, &events)
        .flat_map(move |_| project)
        .map_err(DispatchError::Store)
    })
}

/// Fold `stream_id` with `projection`, then answer `query` with `query_handler`.
pub fn query_projection<Qry, Ans, Ev, S, P, Q, Store>(
  query_handler: &Q,
  store: &Store,
  stream_id: &str,
  projection: P,
  query: Qry,
) -> Effect<Ans, QueryDispatchError<Q::Error>>
where
  Q: QueryHandler<Qry, Ans>,
  Store: EventStore<Ev> + ?Sized,
  P: Projection<S, Ev> + Send + 'static,
  Ev: Clone + Send + Sync + 'static,
  S: Send + 'static,
  Ans: Send + 'static,
{
  let answer = query_handler
    .query(query)
    .map_err(QueryDispatchError::Query);
  run_projection_from_store(store, stream_id, 1, projection)
    .map_err(QueryDispatchError::Store)
    .flat_map(move |_state: S| answer)
}

/// Combined failure for [`dispatch_command`].
#[derive(Debug, thiserror::Error)]
pub enum DispatchError<E: std::error::Error + Send + Sync + 'static> {
  /// Command handler rejected the input.
  #[error("command rejected: {0}")]
  Command(#[source] E),
  /// Event store failed while appending.
  #[error("event store: {0}")]
  Store(#[from] EventStoreError),
}

/// Combined failure for [`query_projection`].
#[derive(Debug, thiserror::Error)]
pub enum QueryDispatchError<E: std::error::Error + Send + Sync + 'static> {
  /// Query handler failed.
  #[error("query failed: {0}")]
  Query(#[source] E),
  /// Event store failed while loading events.
  #[error("event store: {0}")]
  Store(#[from] EventStoreError),
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::edge::run_sync;
  use crate::event_store::MemoryEventStore;
  use id_effect::{fail, succeed};
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  #[derive(Clone, Debug, PartialEq, Eq)]
  enum Evt {
    Deposited(i32),
  }

  #[derive(Debug, PartialEq, Eq)]
  struct Deposit {
    amount: i32,
  }

  #[derive(Debug, PartialEq, Eq)]
  struct BalanceQuery;

  #[derive(Debug)]
  struct CommandReject(&'static str);
  impl std::error::Error for CommandReject {}
  impl std::fmt::Display for CommandReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "{}", self.0)
    }
  }

  struct DepositHandler;

  impl CommandHandler<Deposit, Evt> for DepositHandler {
    type Error = CommandReject;

    fn handle(&self, command: Deposit) -> Effect<Vec<Evt>, Self::Error> {
      if command.amount <= 0 {
        return fail(CommandReject("amount must be positive"));
      }
      succeed(vec![Evt::Deposited(command.amount)])
    }
  }

  struct BalanceProjection;

  impl Projection<i32, Evt> for BalanceProjection {
    fn initial(&self) -> i32 {
      0
    }

    fn apply(&self, state: i32, event: &Evt) -> i32 {
      match event {
        Evt::Deposited(n) => state + n,
      }
    }
  }

  struct BalanceQueryHandler {
    balance: i32,
  }

  impl QueryHandler<BalanceQuery, i32> for BalanceQueryHandler {
    type Error = std::convert::Infallible;

    fn query(&self, _query: BalanceQuery) -> Effect<i32, Self::Error> {
      succeed(self.balance)
    }
  }

  fn deposit(
    store: &MemoryEventStore<Evt>,
    amount: i32,
  ) -> Effect<i32, DispatchError<CommandReject>> {
    dispatch_command(
      &DepositHandler,
      store,
      "acct",
      BalanceProjection,
      Deposit { amount },
    )
  }

  #[test]
  fn command_reject_display() {
    assert_eq!(
      CommandReject("amount must be positive").to_string(),
      "amount must be positive"
    );
  }

  #[test]
  fn dispatch_command_persists_and_projects() {
    let store = MemoryEventStore::new();
    let state = run_sync(deposit(&store, 25)).expect("dispatch");
    assert_eq!(state, 25);
  }

  /// A handler whose effect counts how often it ran.
  struct CountingHandler(Arc<AtomicUsize>);

  impl CommandHandler<Deposit, Evt> for CountingHandler {
    type Error = CommandReject;

    fn handle(&self, command: Deposit) -> Effect<Vec<Evt>, Self::Error> {
      let runs = Arc::clone(&self.0);
      id_effect::suspend(move || {
        runs.fetch_add(1, Ordering::SeqCst);
        succeed(vec![Evt::Deposited(command.amount)])
      })
    }
  }

  #[test]
  fn cqrs_dispatch_is_an_effect() {
    let store = MemoryEventStore::new();
    let runs = Arc::new(AtomicUsize::new(0));
    let handler = CountingHandler(Arc::clone(&runs));
    let program = dispatch_command(
      &handler,
      &store,
      "acct",
      BalanceProjection,
      Deposit { amount: 4 },
    );
    // Building the dispatch neither handles the command nor touches the store.
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(run_sync(store.latest_version("acct")).expect("latest"), 0);
    assert_eq!(run_sync(program).expect("dispatch"), 4);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(run_sync(store.latest_version("acct")).expect("latest"), 1);

    let query = query_projection(
      &BalanceQueryHandler { balance: 4 },
      &store,
      "acct",
      BalanceProjection,
      BalanceQuery,
    );
    assert_eq!(run_sync(query).expect("query"), 4);
  }

  #[test]
  fn dispatch_rejects_invalid_command() {
    let store = MemoryEventStore::new();
    let err = run_sync(deposit(&store, 0)).expect_err("reject");
    assert!(matches!(err, DispatchError::Command(_)));
    assert_eq!(run_sync(store.latest_version("acct")).expect("latest"), 0);
  }

  /// A store whose appends always conflict.
  #[derive(Clone)]
  struct ConflictingStore;

  impl EventStore<Evt> for ConflictingStore {
    fn append(
      &self,
      stream_id: &str,
      _events: &[Evt],
    ) -> Effect<Vec<crate::StoredEvent<Evt>>, EventStoreError> {
      fail(EventStoreError::VersionConflict {
        stream_id: stream_id.to_owned(),
        expected: 0,
        actual: 1,
      })
    }

    fn append_expected(
      &self,
      stream_id: &str,
      _expected_version: u64,
      events: &[Evt],
    ) -> Effect<Vec<crate::StoredEvent<Evt>>, EventStoreError> {
      self.append(stream_id, events)
    }

    fn read(
      &self,
      _stream_id: &str,
      _from_version: u64,
    ) -> Effect<Vec<crate::StoredEvent<Evt>>, EventStoreError> {
      fail(EventStoreError::Io("down".into()))
    }

    fn latest_version(&self, _stream_id: &str) -> Effect<u64, EventStoreError> {
      succeed(1)
    }
  }

  #[test]
  fn dispatch_and_query_surface_store_failures() {
    let err = run_sync(dispatch_command(
      &DepositHandler,
      &ConflictingStore,
      "acct",
      BalanceProjection,
      Deposit { amount: 1 },
    ))
    .expect_err("conflict");
    assert!(matches!(
      err,
      DispatchError::Store(EventStoreError::VersionConflict { .. })
    ));
    assert_eq!(
      run_sync(ConflictingStore.latest_version("acct")).expect("latest"),
      1
    );
    let err = run_sync(query_projection(
      &BalanceQueryHandler { balance: 0 },
      &ConflictingStore,
      "acct",
      BalanceProjection,
      BalanceQuery,
    ))
    .expect_err("read fails");
    assert!(matches!(
      err,
      QueryDispatchError::Store(EventStoreError::Io(_))
    ));
    let err = run_sync(ConflictingStore.append_expected("acct", 0, &[])).expect_err("conflict");
    assert!(matches!(err, EventStoreError::VersionConflict { .. }));
  }

  struct FailQueryHandler;
  impl QueryHandler<BalanceQuery, i32> for FailQueryHandler {
    type Error = std::io::Error;
    fn query(&self, _query: BalanceQuery) -> Effect<i32, Self::Error> {
      fail(std::io::Error::other("query fail"))
    }
  }

  #[test]
  fn query_handler_error() {
    let store = MemoryEventStore::new();
    run_sync(deposit(&store, 5)).expect("seed");
    let err = run_sync(query_projection(
      &FailQueryHandler,
      &store,
      "acct",
      BalanceProjection,
      BalanceQuery,
    ))
    .expect_err("fail");
    assert!(matches!(err, QueryDispatchError::Query(_)));
  }

  #[test]
  fn query_projection_reads_handler() {
    let store = MemoryEventStore::new();
    run_sync(deposit(&store, 10)).expect("seed");
    let ans = run_sync(query_projection(
      &BalanceQueryHandler { balance: 10 },
      &store,
      "acct",
      BalanceProjection,
      BalanceQuery,
    ))
    .expect("query");
    assert_eq!(ans, 10);
  }
}
