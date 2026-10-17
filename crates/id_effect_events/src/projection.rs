//! Fold event streams into read models.

use crate::error::EventStoreError;
use crate::event_store::{EventStore, StoredEvent};
use id_effect::Effect;
use std::sync::Arc;

/// Stateful fold over a domain event type.
pub trait Projection<S, E> {
  /// Initial read-model state.
  fn initial(&self) -> S;
  /// Apply one event to the current state.
  fn apply(&self, state: S, event: &E) -> S;
}

/// A shared projection is a projection (the effects below own the projection they fold with).
impl<S, E, P: Projection<S, E> + ?Sized> Projection<S, E> for Arc<P> {
  fn initial(&self) -> S {
    (**self).initial()
  }

  fn apply(&self, state: S, event: &E) -> S {
    (**self).apply(state, event)
  }
}

/// Fold `events` into a read model using `projection` (pure: no I/O).
pub fn run_projection<S, E, P>(projection: &P, events: impl IntoIterator<Item = E>) -> S
where
  P: Projection<S, E> + ?Sized,
{
  events
    .into_iter()
    .fold(projection.initial(), |state, event| {
      projection.apply(state, &event)
    })
}

/// Load the events of `stream_id` from `from_version` and fold them with `projection`. Nothing
/// is read until the effect runs.
pub fn run_projection_from_store<S, E, P, Store>(
  store: &Store,
  stream_id: &str,
  from_version: u64,
  projection: P,
) -> Effect<S, EventStoreError>
where
  P: Projection<S, E> + Send + 'static,
  Store: EventStore<E> + ?Sized,
  E: Clone + Send + Sync + 'static,
  S: Send + 'static,
{
  store
    .read(stream_id, from_version)
    .map(move |stored: Vec<StoredEvent<E>>| {
      run_projection(&projection, stored.into_iter().map(|s| s.payload))
    })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::edge::run_sync;
  use crate::event_store::MemoryEventStore;

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
    run_sync(store.append("wallet", &[Evt::Added(7), Evt::Added(2)])).expect("append");
    let state = run_sync(run_projection_from_store(&store, "wallet", 1, Balance)).expect("project");
    assert_eq!(state, 9);
  }

  #[test]
  fn run_projection_from_store_is_lazy_and_rereads() {
    let store = MemoryEventStore::new();
    let project = run_projection_from_store(&store, "wallet", 2, Arc::new(Balance));
    run_sync(store.append("wallet", &[Evt::Added(7), Evt::Removed(2)])).expect("append");
    // Built before the append, run after it: the read happens when the effect runs.
    assert_eq!(run_sync(project).expect("project"), -2);
  }
}
