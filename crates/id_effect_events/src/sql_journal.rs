//! SQL-backed event journal schema and test double.

use crate::error::EventStoreError;
use crate::event_store::{EventStore, StoredEvent};
use id_effect::Effect;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// One persisted journal row: `(version, event_id, payload_json)`.
type JournalRow = (u64, String, String);
/// Per-stream journal rows keyed by `stream_id`.
type JournalStore = HashMap<String, Vec<JournalRow>>;

/// DDL for PostgreSQL event journal (apply in your migration runner).
pub const POSTGRES_JOURNAL_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS event_journal (
  stream_id TEXT NOT NULL,
  version BIGINT NOT NULL,
  event_id TEXT NOT NULL,
  payload JSONB NOT NULL,
  PRIMARY KEY (stream_id, version)
);
CREATE INDEX IF NOT EXISTS event_journal_stream_idx ON event_journal (stream_id, version);
"#;

/// Low-level SQL journal operations (implement with `id_effect_sql` in apps).
/// SQL journal persistence port.
pub trait SqlJournalBackend: Send + Sync {
  /// Insert one journal row.
  fn insert_row(
    &self,
    stream_id: &str,
    version: u64,
    event_id: &str,
    payload_json: &str,
  ) -> Result<(), EventStoreError>;
  /// Read rows from `from_version` inclusive.
  fn select_from(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Result<Vec<JournalRow>, EventStoreError>;
}

/// In-memory backend matching the SQL journal shape.
#[derive(Clone, Default)]
pub struct TestSqlJournalBackend {
  rows: Arc<Mutex<JournalStore>>,
}

impl SqlJournalBackend for TestSqlJournalBackend {
  fn insert_row(
    &self,
    stream_id: &str,
    version: u64,
    event_id: &str,
    payload_json: &str,
  ) -> Result<(), EventStoreError> {
    let mut guard = self
      .rows
      .lock()
      .map_err(|e| EventStoreError::Io(e.to_string()))?;
    let stream = guard.entry(stream_id.to_owned()).or_default();
    if stream.iter().any(|(v, _, _)| *v == version) {
      return Err(EventStoreError::VersionConflict {
        stream_id: stream_id.to_owned(),
        expected: version.saturating_sub(1),
        actual: version,
      });
    }
    stream.push((version, event_id.to_owned(), payload_json.to_owned()));
    stream.sort_by_key(|(v, _, _)| *v);
    Ok(())
  }

  fn select_from(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Result<Vec<JournalRow>, EventStoreError> {
    let guard = self
      .rows
      .lock()
      .map_err(|e| EventStoreError::Io(e.to_string()))?;
    Ok(
      guard
        .get(stream_id)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|(v, _, _)| *v >= from_version)
        .collect(),
    )
  }
}

/// [`EventStore`] backed by [`SqlJournalBackend`].
pub struct SqlEventJournal<E> {
  backend: Arc<dyn SqlJournalBackend>,
  _marker: std::marker::PhantomData<E>,
}

impl<E: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> SqlEventJournal<E> {
  /// Wrap `backend` as an [`EventStore`].
  pub fn new(backend: Arc<dyn SqlJournalBackend>) -> Self {
    Self {
      backend,
      _marker: std::marker::PhantomData,
    }
  }
}

impl<E: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> EventStore<E>
  for SqlEventJournal<E>
{
  fn append(
    &self,
    stream_id: &str,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let events: Vec<E> = events.to_vec();
    let backend = Arc::clone(&self.backend);
    Effect::new(move |_r| {
      let latest = backend
        .select_from(&stream_id, 1)?
        .iter()
        .map(|(v, _, _)| *v)
        .max()
        .unwrap_or(0);
      let mut out = Vec::with_capacity(events.len());
      for (i, payload) in events.into_iter().enumerate() {
        let version = latest + i as u64 + 1;
        let event_id = Uuid::new_v4().to_string();
        let json =
          serde_json::to_string(&payload).map_err(|e| EventStoreError::Io(e.to_string()))?;
        backend.insert_row(&stream_id, version, &event_id, &json)?;
        out.push(StoredEvent {
          event_id,
          version,
          payload,
        });
      }
      Ok(out)
    })
  }

  fn read(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let backend = Arc::clone(&self.backend);
    Effect::new(move |_r| {
      let rows = backend.select_from(&stream_id, from_version)?;
      rows
        .into_iter()
        .map(|(version, event_id, json)| {
          let payload: E =
            serde_json::from_str(&json).map_err(|e| EventStoreError::Io(e.to_string()))?;
          Ok(StoredEvent {
            event_id,
            version,
            payload,
          })
        })
        .collect()
    })
  }

  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let backend = Arc::clone(&self.backend);
    Effect::new(move |_r| {
      Ok(
        backend
          .select_from(&stream_id, 1)?
          .into_iter()
          .map(|(v, _, _)| v)
          .max()
          .unwrap_or(0),
      )
    })
  }
}
