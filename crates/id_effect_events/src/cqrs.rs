//! CQRS boundary types — command handlers produce events; query handlers read projections.

use crate::error::EventStoreError;
use crate::event_store::EventStore;
use crate::projection::{Projection, run_projection_from_store};
use id_effect::{Effect, run_blocking};

/// Turn a command into zero or more domain events (write side).
pub trait CommandHandler<Cmd, Ev> {
  /// Error type for rejected commands.
  type Error: std::error::Error + Send + Sync + 'static;
  /// Validate and map `command` to events without persisting.
  fn handle(&self, command: Cmd) -> Effect<Vec<Ev>, Self::Error, ()>;
}

/// Answer a query from a materialized read model (read side).
pub trait QueryHandler<Qry, Ans> {
  /// Error type for failed queries.
  type Error: std::error::Error + Send + Sync + 'static;
  /// Run the query.
  fn query(&self, query: Qry) -> Effect<Ans, Self::Error, ()>;
}

/// Persist command-generated events then return the updated projection state.
pub fn dispatch_command<Cmd, Ev, S, P, H, Store>(
  handler: &H,
  store: &Store,
  stream_id: &str,
  projection: &P,
  command: Cmd,
) -> Result<S, DispatchError<H::Error>>
where
  H: CommandHandler<Cmd, Ev>,
  Store: EventStore<Ev>,
  P: Projection<S, Ev>,
  Cmd: Send + 'static,
  Ev: Clone + Send + Sync + 'static,
  S: Send + 'static,
{
  let events = run_blocking(handler.handle(command), ()).map_err(DispatchError::Command)?;
  run_blocking(store.append(stream_id, &events), ()).map_err(DispatchError::Store)?;
  run_projection_from_store(store, stream_id, 1, projection).map_err(DispatchError::Store)
}

/// Query a projection rebuilt from the event store.
pub fn query_projection<Qry, Ans, Ev, S, P, Q, Store>(
  query_handler: &Q,
  store: &Store,
  stream_id: &str,
  projection: &P,
  query: Qry,
) -> Result<Ans, QueryDispatchError<Q::Error>>
where
  Q: QueryHandler<Qry, Ans>,
  Store: EventStore<Ev>,
  P: Projection<S, Ev>,
  Qry: Send + 'static,
  Ev: Clone + Send + Sync + 'static,
  S: Send + 'static,
  Ans: Send + 'static,
{
  let _state = run_projection_from_store(store, stream_id, 1, projection)
    .map_err(QueryDispatchError::Store)?;
  run_blocking(
    query_handler
      .query(query)
      .map_error(QueryDispatchError::Query),
    (),
  )
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
  use crate::event_store::MemoryEventStore;
  use id_effect::{fail, succeed};

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

    fn handle(&self, command: Deposit) -> Effect<Vec<Evt>, Self::Error, ()> {
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

    fn query(&self, _query: BalanceQuery) -> Effect<i32, Self::Error, ()> {
      succeed(self.balance)
    }
  }

  #[test]
  fn dispatch_command_persists_and_projects() {
    let store = MemoryEventStore::new();
    let state = dispatch_command(
      &DepositHandler,
      &store,
      "acct",
      &BalanceProjection,
      Deposit { amount: 25 },
    )
    .expect("dispatch");
    assert_eq!(state, 25);
  }

  #[test]
  fn dispatch_rejects_invalid_command() {
    let store = MemoryEventStore::new();
    let err = dispatch_command(
      &DepositHandler,
      &store,
      "acct",
      &BalanceProjection,
      Deposit { amount: 0 },
    )
    .expect_err("reject");
    assert!(matches!(err, DispatchError::Command(_)));
  }

  #[test]
  fn query_projection_reads_handler() {
    let store = MemoryEventStore::new();
    dispatch_command(
      &DepositHandler,
      &store,
      "acct",
      &BalanceProjection,
      Deposit { amount: 10 },
    )
    .expect("seed");
    let ans = query_projection(
      &BalanceQueryHandler { balance: 10 },
      &store,
      "acct",
      &BalanceProjection,
      BalanceQuery,
    )
    .expect("query");
    assert_eq!(ans, 10);
  }
}
