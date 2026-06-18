//! Fold event streams into read models.

use crate::error::EventStoreError;
use crate::event_store::{EventStore, StoredEvent};
use id_effect::run_blocking;

/// Stateful fold over a domain event type.
pub trait Projection<S, E> {
  /// Initial read-model state.
  fn initial(&self) -> S;
  /// Apply one event to the current state.
  fn apply(&self, state: S, event: &E) -> S;
}

/// Fold `events` into a read model using `projection`.
pub fn run_projection<S, E, P>(projection: &P, events: impl IntoIterator<Item = E>) -> S
where
  P: Projection<S, E>,
{
  events
    .into_iter()
    .fold(projection.initial(), |state, event| {
      projection.apply(state, &event)
    })
}

/// Load events from `store` and fold them with `projection`.
pub fn run_projection_from_store<S, E, P, Store>(
  store: &Store,
  stream_id: &str,
  from_version: u64,
  projection: &P,
) -> Result<S, EventStoreError>
where
  P: Projection<S, E>,
  Store: EventStore<E>,
  E: Clone + Send + Sync + 'static,
{
  let stored = run_blocking(store.read(stream_id, from_version), ())?;
  let events = stored.into_iter().map(|s: StoredEvent<E>| s.payload);
  Ok(run_projection(projection, events))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::event_store::MemoryEventStore;
  use id_effect::run_blocking;

  #[derive(Clone, Debug, PartialEq, Eq)]
  enum Evt {
    Added(i32),
    Removed(i32),
  }

  struct Balance;

  impl Projection<i32, Evt> for Balance {
    fn initial(&self) -> i32 {
      0
    }

    fn apply(&self, state: i32, event: &Evt) -> i32 {
      match event {
        Evt::Added(n) => state + n,
        Evt::Removed(n) => state - n,
      }
    }
  }

  #[test]
  fn run_projection_folds_events() {
    let events = [Evt::Added(10), Evt::Removed(3), Evt::Added(5)];
    let state = run_projection(&Balance, events);
    assert_eq!(state, 12);
  }

  #[test]
  fn run_projection_from_store_reads_stream() {
    let store = MemoryEventStore::new();
    run_blocking(store.append("wallet", &[Evt::Added(7), Evt::Added(2)]), ()).expect("append");
    let state = run_projection_from_store(&store, "wallet", 1, &Balance).expect("project");
    assert_eq!(state, 9);
  }
}
