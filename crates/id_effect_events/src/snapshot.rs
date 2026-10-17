//! Aggregate snapshots: a stream's folded state stored at a version, so loading the aggregate
//! reads only the events after it.
//!
//! [`load_aggregate`] folds the latest [`Snapshot`] from a [`SnapshotStore`] (or the
//! projection's initial state) with the stream's later events; the result is itself a
//! [`Snapshot`] at the stream's current version, ready for
//! [`EventStore::append_expected`](crate::EventStore::append_expected) and for
//! [`SnapshotStore::save`] when the caller decides it is time (every N events, on a timer, …).
//! A snapshot is a cache: deleting it changes how much is read, never the state loaded.

use crate::edge::attempt;
use crate::error::EventStoreError;
use crate::event_store::{EventStore, StoredEvent};
use crate::projection::Projection;
use crate::sql_journal::{ready, to_sql_version, to_version};
use id_effect::{Effect, fail};
use id_effect_sql::{SqlClient, SqlClientService, SqlRow, params};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A stream's state folded up to `version` (`0`: no events, the projection's initial state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot<S> {
  /// The stream.
  pub stream_id: String,
  /// The last version folded into `state`.
  pub version: u64,
  /// The folded state.
  pub state: S,
}

/// Latest snapshot per stream.
///
/// One snapshot per stream id: an aggregate type per stream, as event-sourced streams usually
/// are (`account-1`, `order-7`). A saved snapshot replaces an older one; saving one older than
/// the stored one is a no-op, so a slow writer cannot move a stream's snapshot backwards.
pub trait SnapshotStore<S>: Send + Sync {
  /// Store `snapshot` unless the stream already has one at a later version.
  fn save(&self, snapshot: &Snapshot<S>) -> Effect<(), EventStoreError>;
  /// The stream's latest snapshot, if any.
  fn load(&self, stream_id: &str) -> Effect<Option<Snapshot<S>>, EventStoreError>;
}

/// A shared snapshot store is a snapshot store.
impl<S, T: SnapshotStore<S> + ?Sized> SnapshotStore<S> for Arc<T> {
  fn save(&self, snapshot: &Snapshot<S>) -> Effect<(), EventStoreError> {
    (**self).save(snapshot)
  }

  fn load(&self, stream_id: &str) -> Effect<Option<Snapshot<S>>, EventStoreError> {
    (**self).load(stream_id)
  }
}

/// Load the aggregate of `stream_id`: its latest snapshot, then the events after the snapshot's
/// version folded with `projection` (without a snapshot: every event from the projection's
/// initial state). Succeeds with the state at the stream's current version. Nothing is read
/// until the effect runs.
pub fn load_aggregate<S, E, P, Store, Snaps>(
  store: &Store,
  snapshots: &Snaps,
  stream_id: &str,
  projection: P,
) -> Effect<Snapshot<S>, EventStoreError>
where
  P: Projection<S, E> + Send + 'static,
  Store: EventStore<E> + Clone + 'static,
  Snaps: SnapshotStore<S> + ?Sized,
  E: Clone + Send + Sync + 'static,
  S: Send + 'static,
{
  let store = store.clone();
  let stream_id = stream_id.to_owned();
  snapshots.load(&stream_id).flat_map(move |snapshot| {
    let (from, state) = match snapshot {
      Some(snapshot) => (snapshot.version, snapshot.state),
      None => (0, projection.initial()),
    };
    store
      .read(&stream_id, from + 1)
      .map(move |tail: Vec<StoredEvent<E>>| {
        let version = tail.last().map_or(from, |e| e.version);
        let state = tail
          .iter()
          .fold(state, |state, e| projection.apply(state, &e.payload));
        Snapshot {
          stream_id,
          version,
          state,
        }
      })
  })
}

/// In-memory snapshots (tests). Clones share the same snapshots.
pub struct MemorySnapshotStore<S> {
  snapshots: Arc<Mutex<HashMap<String, Snapshot<S>>>>,
}

impl<S> Clone for MemorySnapshotStore<S> {
  fn clone(&self) -> Self {
    Self {
      snapshots: Arc::clone(&self.snapshots),
    }
  }
}

impl<S> MemorySnapshotStore<S> {
  /// No snapshots.
  pub fn new() -> Self {
    Self {
      snapshots: Arc::new(Mutex::new(HashMap::new())),
    }
  }
}

impl<S> Default for MemorySnapshotStore<S> {
  fn default() -> Self {
    Self::new()
  }
}

impl<S: Clone + Send + Sync + 'static> SnapshotStore<S> for MemorySnapshotStore<S> {
  fn save(&self, snapshot: &Snapshot<S>) -> Effect<(), EventStoreError> {
    let (snapshots, snapshot) = (Arc::clone(&self.snapshots), snapshot.clone());
    attempt(move || {
      let mut guard = snapshots
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      let newer = guard
        .get(&snapshot.stream_id)
        .is_none_or(|stored| snapshot.version > stored.version);
      if newer {
        guard.insert(snapshot.stream_id.clone(), snapshot);
      }
      Ok(())
    })
  }

  fn load(&self, stream_id: &str) -> Effect<Option<Snapshot<S>>, EventStoreError> {
    let (snapshots, stream_id) = (Arc::clone(&self.snapshots), stream_id.to_owned());
    attempt(move || {
      let guard = snapshots
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      Ok(guard.get(&stream_id).cloned())
    })
  }
}

/// DDL of the snapshot table (portable PostgreSQL / SQLite; states are JSON text). Apply it in
/// a migration, or at startup with [`SqlSnapshotStore::create_table`].
pub const SQL_SNAPSHOT_DDL: &str = "CREATE TABLE IF NOT EXISTS id_effect_event_snapshot (
  stream_id TEXT PRIMARY KEY,
  version BIGINT NOT NULL,
  state TEXT NOT NULL
)";

const SAVE_SNAPSHOT_SQL: &str = "INSERT INTO id_effect_event_snapshot (stream_id, version, state) \
  VALUES ($1, $2, $3) ON CONFLICT (stream_id) DO UPDATE \
  SET version = excluded.version, state = excluded.state \
  WHERE excluded.version > id_effect_event_snapshot.version";

const LOAD_SNAPSHOT_SQL: &str =
  "SELECT version, state FROM id_effect_event_snapshot WHERE stream_id = $1";

/// [`SnapshotStore`] in the table of [`SQL_SNAPSHOT_DDL`], through a [`SqlClient`]; states are
/// stored as JSON. Clones share the client.
pub struct SqlSnapshotStore<S> {
  client: Arc<dyn SqlClient>,
  _marker: std::marker::PhantomData<fn() -> S>,
}

impl<S> Clone for SqlSnapshotStore<S> {
  fn clone(&self) -> Self {
    Self {
      client: Arc::clone(&self.client),
      _marker: std::marker::PhantomData,
    }
  }
}

impl<S> SqlSnapshotStore<S> {
  /// Snapshots over `client`.
  pub fn new(client: Arc<dyn SqlClient>) -> Self {
    Self {
      client,
      _marker: std::marker::PhantomData,
    }
  }

  /// Snapshots over the client of a [`SqlClientService`].
  pub fn from_service(service: &SqlClientService) -> Self {
    Self::new(Arc::clone(&service.0))
  }

  /// Run [`SQL_SNAPSHOT_DDL`] (idempotent).
  pub fn create_table(&self) -> Effect<(), EventStoreError> {
    self
      .client
      .execute(SQL_SNAPSHOT_DDL, &[])
      .map(|_| ())
      .map_err(EventStoreError::from)
  }
}

fn decode_snapshot<S: DeserializeOwned>(
  stream_id: String,
  row: &SqlRow,
) -> Result<Snapshot<S>, EventStoreError> {
  let json = row.get::<String>("state")?;
  Ok(Snapshot {
    stream_id,
    version: to_version(row.get::<i64>("version")?)?,
    state: serde_json::from_str(&json).map_err(|e| EventStoreError::Serde(e.to_string()))?,
  })
}

impl<S: Serialize + DeserializeOwned + Send + Sync + 'static> SnapshotStore<S>
  for SqlSnapshotStore<S>
{
  fn save(&self, snapshot: &Snapshot<S>) -> Effect<(), EventStoreError> {
    let encoded = serde_json::to_string(&snapshot.state)
      .map_err(|e| EventStoreError::Serde(e.to_string()))
      .and_then(|json| Ok((to_sql_version(snapshot.version)?, json)));
    let (version, json) = match encoded {
      Ok(encoded) => encoded,
      Err(e) => return fail(e),
    };
    self
      .client
      .execute(
        SAVE_SNAPSHOT_SQL,
        &params![snapshot.stream_id.as_str(), version, json],
      )
      .map(|_| ())
      .map_err(EventStoreError::from)
  }

  fn load(&self, stream_id: &str) -> Effect<Option<Snapshot<S>>, EventStoreError> {
    let owned = stream_id.to_owned();
    self
      .client
      .query(LOAD_SNAPSHOT_SQL, &params![stream_id])
      .map_err(EventStoreError::from)
      .flat_map(move |rows| {
        ready(
          rows
            .first()
            .map(|row| decode_snapshot(owned, row))
            .transpose(),
        )
      })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::edge::run_sync;
  use crate::event_store::MemoryEventStore;
  use id_effect_sql::{SqlValue, TestSqlClient};
  use std::sync::atomic::{AtomicUsize, Ordering};

  #[derive(Clone, Debug, PartialEq, Eq)]
  enum Evt {
    Deposited(i64),
    Withdrawn(i64),
  }

  /// The balance, counting how many events it folded.
  struct Balance(Arc<AtomicUsize>);

  impl Projection<i64, Evt> for Balance {
    fn initial(&self) -> i64 {
      0
    }
    fn apply(&self, balance: i64, event: &Evt) -> i64 {
      self.0.fetch_add(1, Ordering::SeqCst);
      match event {
        Evt::Deposited(n) => balance + n,
        Evt::Withdrawn(n) => balance - n,
      }
    }
  }

  #[test]
  fn aggregate_loads_from_snapshot_plus_tail() {
    let store = MemoryEventStore::new();
    let snapshots = MemorySnapshotStore::new();
    let folded = Arc::new(AtomicUsize::new(0));
    run_sync(store.append(
      "acct",
      &[Evt::Deposited(100), Evt::Withdrawn(30), Evt::Deposited(5)],
    ))
    .expect("append");

    // No snapshot: every event is folded.
    let full = run_sync(load_aggregate(
      &store,
      &snapshots,
      "acct",
      Balance(Arc::clone(&folded)),
    ))
    .expect("load");
    assert_eq!((full.version, full.state), (3, 75));
    assert_eq!(folded.swap(0, Ordering::SeqCst), 3);
    run_sync(snapshots.save(&full)).expect("snapshot");

    run_sync(store.append_expected("acct", 3, &[Evt::Withdrawn(25), Evt::Deposited(1)]))
      .expect("append after snapshot");
    let loaded = run_sync(load_aggregate(
      &store,
      &snapshots,
      "acct",
      Balance(Arc::clone(&folded)),
    ))
    .expect("load");
    // Snapshot state at version 3, plus only the two later events.
    assert_eq!(
      loaded,
      Snapshot {
        stream_id: "acct".into(),
        version: 5,
        state: 51
      }
    );
    assert_eq!(folded.load(Ordering::SeqCst), 2);
  }

  #[test]
  fn snapshot_is_only_a_cache() {
    let store = MemoryEventStore::new();
    let folded = Arc::new(AtomicUsize::new(0));
    run_sync(store.append("acct", &[Evt::Deposited(10), Evt::Deposited(2)])).expect("append");
    let with = MemorySnapshotStore::default();
    run_sync(with.save(&Snapshot {
      stream_id: "acct".into(),
      version: 1,
      state: 10,
    }))
    .expect("snapshot");
    let a = run_sync(load_aggregate(
      &store,
      &with,
      "acct",
      Balance(Arc::clone(&folded)),
    ))
    .expect("with snapshot");
    let b = run_sync(load_aggregate(
      &store,
      &MemorySnapshotStore::new(),
      "acct",
      Balance(Arc::clone(&folded)),
    ))
    .expect("without");
    assert_eq!(a, b);
    let empty = run_sync(load_aggregate(
      &store,
      &with,
      "nobody",
      Balance(Arc::clone(&folded)),
    ))
    .expect("empty stream");
    assert_eq!((empty.version, empty.state), (0, 0));
  }

  #[test]
  fn older_snapshot_does_not_replace_newer() {
    let snapshots = Arc::new(MemorySnapshotStore::new());
    let at = |version, state| Snapshot {
      stream_id: "s".to_owned(),
      version,
      state,
    };
    run_sync(snapshots.save(&at(4, 40))).expect("save 4");
    run_sync(snapshots.clone().save(&at(2, 20))).expect("save 2");
    assert_eq!(
      run_sync(snapshots.load("s")).expect("load"),
      Some(at(4, 40))
    );
    run_sync(snapshots.save(&at(6, 60))).expect("save 6");
    assert_eq!(
      run_sync(snapshots.load("s")).expect("load"),
      Some(at(6, 60))
    );
    assert_eq!(run_sync(snapshots.load("t")).expect("none"), None);
  }

  #[test]
  fn sql_snapshots_encode_json_and_decode_rows() {
    let test = TestSqlClient::new();
    run_sync(test.connect()).expect("connect");
    let snapshots =
      SqlSnapshotStore::<Vec<u32>>::from_service(&SqlClientService::new(test.clone()));
    run_sync(snapshots.create_table()).expect("ddl");
    run_sync(snapshots.clone().save(&Snapshot {
      stream_id: "s".into(),
      version: 3,
      state: vec![1, 2],
    }))
    .expect("save");
    let executes = test.recorded_executes();
    assert_eq!(executes[0].0, SQL_SNAPSHOT_DDL);
    assert_eq!(
      executes[1].1,
      [
        SqlValue::Text("s".into()),
        SqlValue::I64(3),
        SqlValue::Text("[1,2]".into())
      ]
    );

    assert_eq!(run_sync(snapshots.load("s")).expect("none"), None);
    test.script_query(
      LOAD_SNAPSHOT_SQL,
      vec![SqlRow::from_pairs([
        ("version", SqlValue::I64(3)),
        ("state", SqlValue::Text("[1,2]".into())),
      ])],
    );
    assert_eq!(
      run_sync(snapshots.load("s")).expect("load"),
      Some(Snapshot {
        stream_id: "s".into(),
        version: 3,
        state: vec![1, 2]
      })
    );
    test.script_query(
      LOAD_SNAPSHOT_SQL,
      vec![SqlRow::from_pairs([
        ("version", SqlValue::I64(3)),
        ("state", SqlValue::Text("nope".into())),
      ])],
    );
    assert!(matches!(
      run_sync(snapshots.load("s")).expect_err("serde"),
      EventStoreError::Serde(_)
    ));
    assert!(matches!(
      run_sync(snapshots.save(&Snapshot {
        stream_id: "s".into(),
        version: u64::MAX,
        state: vec![],
      }))
      .expect_err("range"),
      EventStoreError::Io(_)
    ));
  }
}
