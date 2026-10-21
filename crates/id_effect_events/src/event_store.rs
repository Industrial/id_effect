//! [`EventStore`] trait and in-memory / file-backed implementations.

use crate::edge::attempt;
use crate::error::EventStoreError;
use crate::subscription::{EventLog, RecordedEvent};
use id_effect::Effect;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// One persisted event with monotonic stream version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvent<E> {
  /// Unique event id.
  pub event_id: String,
  /// Monotonic version within the stream (1-based).
  pub version: u64,
  /// Domain payload.
  pub payload: E,
}

/// Append-only event persistence.
///
/// A stream's version is the number of events in it (`0` when empty); its events carry the
/// versions `1..=version`.
pub trait EventStore<E>: Send + Sync {
  /// Append events to `stream_id` at its current version, returning assigned versions. A writer
  /// that appends the same versions first fails this append with
  /// [`EventStoreError::VersionConflict`] (stores that detect it).
  fn append(&self, stream_id: &str, events: &[E]) -> Effect<Vec<StoredEvent<E>>, EventStoreError>;

  /// Append events to `stream_id` only if the stream is at `expected_version` (optimistic
  /// concurrency). Otherwise nothing is appended and the effect fails with
  /// [`EventStoreError::VersionConflict`] naming the version found. The events get the versions
  /// `expected_version + 1 ..`.
  fn append_expected(
    &self,
    stream_id: &str,
    expected_version: u64,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError>;

  /// Read events from `stream_id` starting at `from_version` (inclusive, 1-based).
  fn read(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError>;

  /// Latest assigned version, or `0` when the stream is empty.
  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError>;
}

/// A shared store is a store (handlers and dispatch take stores that are `Clone`).
impl<E, S: EventStore<E> + ?Sized> EventStore<E> for Arc<S> {
  fn append(&self, stream_id: &str, events: &[E]) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    (**self).append(stream_id, events)
  }

  fn append_expected(
    &self,
    stream_id: &str,
    expected_version: u64,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    (**self).append_expected(stream_id, expected_version, events)
  }

  fn read(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    (**self).read(stream_id, from_version)
  }

  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError> {
    (**self).latest_version(stream_id)
  }
}

/// [`EventStoreError::VersionConflict`] unless `actual == expected` (no expectation: `None`).
pub(crate) fn check_version(
  stream_id: &str,
  expected: Option<u64>,
  actual: u64,
) -> Result<(), EventStoreError> {
  match expected {
    Some(expected) if expected != actual => Err(EventStoreError::VersionConflict {
      stream_id: stream_id.to_owned(),
      expected,
      actual,
    }),
    _ => Ok(()),
  }
}

/// In-memory append-only store (tests and ephemeral demos). Clones share the same streams.
///
/// It is also an [`EventLog`](crate::EventLog): every append gets the next global position.
pub struct MemoryEventStore<E> {
  streams: Arc<Mutex<MemoryStreams<E>>>,
}

/// The streams, and the global log of appends as `(stream_id, version)`; position `n` is
/// `log[n - 1]`.
struct MemoryStreams<E> {
  by_id: HashMap<String, Vec<StoredEvent<E>>>,
  log: Vec<(String, u64)>,
}

impl<E> Clone for MemoryEventStore<E> {
  fn clone(&self) -> Self {
    Self {
      streams: Arc::clone(&self.streams),
    }
  }
}

impl<E> MemoryEventStore<E> {
  /// Empty store.
  pub fn new() -> Self {
    Self {
      streams: Arc::new(Mutex::new(MemoryStreams {
        by_id: HashMap::new(),
        log: Vec::new(),
      })),
    }
  }
}

impl<E> Default for MemoryEventStore<E> {
  fn default() -> Self {
    Self::new()
  }
}

impl<E: Clone + Send + Sync + 'static> MemoryEventStore<E> {
  /// Appends under the lock, so the version check and the append are atomic.
  fn append_at(
    &self,
    stream_id: &str,
    expected: Option<u64>,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    let stream_id = stream_id.to_owned();
    let events: Vec<E> = events.to_vec();
    let streams = Arc::clone(&self.streams);
    attempt(move || {
      let mut guard = streams
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      let MemoryStreams { by_id, log } = &mut *guard;
      let current = by_id.get(&stream_id).map_or(0, |s| s.len() as u64);
      check_version(&stream_id, expected, current)?;
      let stream = by_id.entry(stream_id.clone()).or_default();
      let mut out = Vec::with_capacity(events.len());
      for payload in events {
        let version = stream.len() as u64 + 1;
        let stored = StoredEvent {
          event_id: Uuid::new_v4().to_string(),
          version,
          payload,
        };
        stream.push(stored.clone());
        log.push((stream_id.clone(), version));
        out.push(stored);
      }
      Ok(out)
    })
  }
}

impl<E: Clone + Send + Sync + 'static> EventStore<E> for MemoryEventStore<E> {
  fn append(&self, stream_id: &str, events: &[E]) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    self.append_at(stream_id, None, events)
  }

  fn append_expected(
    &self,
    stream_id: &str,
    expected_version: u64,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    self.append_at(stream_id, Some(expected_version), events)
  }

  fn read(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    let stream_id = stream_id.to_owned();
    let streams = Arc::clone(&self.streams);
    attempt(move || {
      let guard = streams
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      let Some(stream) = guard.by_id.get(&stream_id) else {
        return Ok(Vec::new());
      };
      Ok(
        stream
          .iter()
          .filter(|e| e.version >= from_version)
          .cloned()
          .collect(),
      )
    })
  }

  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError> {
    let stream_id = stream_id.to_owned();
    let streams = Arc::clone(&self.streams);
    attempt(move || {
      let guard = streams
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      Ok(
        guard
          .by_id
          .get(&stream_id)
          .map(|s| s.len() as u64)
          .unwrap_or(0),
      )
    })
  }
}

impl<E: Clone + Send + Sync + 'static> EventLog<E> for MemoryEventStore<E> {
  fn read_all(&self, after: u64, limit: usize) -> Effect<Vec<RecordedEvent<E>>, EventStoreError> {
    let streams = Arc::clone(&self.streams);
    attempt(move || {
      let guard = streams
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      let skip = usize::try_from(after).unwrap_or(usize::MAX);
      Ok(
        guard
          .log
          .iter()
          .enumerate()
          .skip(skip)
          .take(limit)
          .filter_map(|(i, (stream_id, version))| {
            let event = guard.by_id.get(stream_id)?.get(*version as usize - 1)?;
            Some(RecordedEvent {
              position: i as u64 + 1,
              stream_id: stream_id.clone(),
              event: event.clone(),
            })
          })
          .collect(),
      )
    })
  }

  /// The number of events appended (positions are gap-free from 1), in O(1).
  fn head(&self) -> Effect<u64, EventStoreError> {
    let streams = Arc::clone(&self.streams);
    attempt(move || {
      let guard = streams
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      Ok(guard.log.len() as u64)
    })
  }
}

/// JSON-lines journal on disk (one [`StoredEvent`] per line). One writer process at a time:
/// the version check of [`EventStore::append_expected`] is not atomic across processes.
pub struct FileJournal<E> {
  path: PathBuf,
  _marker: std::marker::PhantomData<E>,
}

impl<E> Clone for FileJournal<E> {
  fn clone(&self) -> Self {
    Self {
      path: self.path.clone(),
      _marker: std::marker::PhantomData,
    }
  }
}

impl<E: Serialize + DeserializeOwned + Send + Sync + 'static> FileJournal<E> {
  /// Open or create a journal at `path`.
  pub fn open(path: impl AsRef<Path>) -> Result<Self, EventStoreError> {
    let path = path.as_ref().to_path_buf();
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).map_err(|e| EventStoreError::Io(e.to_string()))?;
    }
    OpenOptions::new()
      .create(true)
      .append(true)
      .open(&path)
      .map_err(|e| EventStoreError::Io(e.to_string()))?;
    Ok(Self {
      path,
      _marker: std::marker::PhantomData,
    })
  }

  fn read_all(&self) -> Result<HashMap<String, Vec<StoredEvent<E>>>, EventStoreError> {
    let file = File::open(&self.path).map_err(|e| EventStoreError::Io(e.to_string()))?;
    let reader = BufReader::new(file);
    let mut streams: HashMap<String, Vec<StoredEvent<E>>> = HashMap::new();
    for line in reader.lines() {
      let line = line.map_err(|e| EventStoreError::Io(e.to_string()))?;
      if line.trim().is_empty() {
        continue;
      }
      let record: JournalRecord<E> =
        serde_json::from_str(&line).map_err(|e| EventStoreError::Serde(e.to_string()))?;
      streams
        .entry(record.stream_id)
        .or_default()
        .push(StoredEvent {
          event_id: record.event_id,
          version: record.version,
          payload: record.payload,
        });
    }
    for stream in streams.values_mut() {
      stream.sort_by_key(|e| e.version);
    }
    Ok(streams)
  }

  fn append_records(&self, records: &[JournalRecord<E>]) -> Result<(), EventStoreError> {
    let mut file = OpenOptions::new()
      .append(true)
      .open(&self.path)
      .map_err(|e| EventStoreError::Io(e.to_string()))?;
    for record in records {
      let line =
        serde_json::to_string(record).map_err(|e| EventStoreError::Serde(e.to_string()))?;
      writeln!(file, "{line}").map_err(|e| EventStoreError::Io(e.to_string()))?;
    }
    Ok(())
  }
}

#[derive(Serialize, Deserialize)]
struct JournalRecord<E> {
  stream_id: String,
  event_id: String,
  version: u64,
  payload: E,
}

impl<E: Clone + Serialize + DeserializeOwned + Send + Sync + 'static> FileJournal<E> {
  fn append_at(
    &self,
    stream_id: &str,
    expected: Option<u64>,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    let stream_id = stream_id.to_owned();
    let events: Vec<E> = events.to_vec();
    let path = self.path.clone();
    attempt(move || {
      let journal = FileJournal::<E> {
        path,
        _marker: std::marker::PhantomData,
      };
      let mut streams = journal.read_all()?;
      let current = streams.get(&stream_id).map_or(0, |s| s.len() as u64);
      check_version(&stream_id, expected, current)?;
      let stream = streams.entry(stream_id.clone()).or_default();
      let mut out = Vec::with_capacity(events.len());
      let mut records = Vec::with_capacity(events.len());
      for payload in events {
        let version = stream.len() as u64 + 1;
        let event_id = Uuid::new_v4().to_string();
        let stored = StoredEvent {
          event_id: event_id.clone(),
          version,
          payload: payload.clone(),
        };
        records.push(JournalRecord {
          stream_id: stream_id.clone(),
          event_id,
          version,
          payload,
        });
        stream.push(stored.clone());
        out.push(stored);
      }
      journal.append_records(&records)?;
      Ok(out)
    })
  }
}

impl<E: Clone + Serialize + DeserializeOwned + Send + Sync + 'static> EventStore<E>
  for FileJournal<E>
{
  fn append(&self, stream_id: &str, events: &[E]) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    self.append_at(stream_id, None, events)
  }

  fn append_expected(
    &self,
    stream_id: &str,
    expected_version: u64,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    self.append_at(stream_id, Some(expected_version), events)
  }

  fn read(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    let stream_id = stream_id.to_owned();
    let path = self.path.clone();
    attempt(move || {
      let journal = FileJournal::<E> {
        path,
        _marker: std::marker::PhantomData,
      };
      let streams = journal.read_all()?;
      Ok(
        streams
          .get(&stream_id)
          .cloned()
          .unwrap_or_default()
          .into_iter()
          .filter(|e| e.version >= from_version)
          .collect(),
      )
    })
  }

  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError> {
    let stream_id = stream_id.to_owned();
    let path = self.path.clone();
    attempt(move || {
      let journal = FileJournal::<E> {
        path,
        _marker: std::marker::PhantomData,
      };
      let streams = journal.read_all()?;
      Ok(streams.get(&stream_id).map(|s| s.len() as u64).unwrap_or(0))
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::edge::run_sync;

  #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
  struct CounterEvt(u32);

  #[test]
  fn memory_store_appends_and_reads() {
    let store = MemoryEventStore::<CounterEvt>::new();
    let stored = run_sync(store.append("s1", &[CounterEvt(1), CounterEvt(2)])).expect("append");
    assert_eq!(stored[0].version, 1);
    assert_eq!(stored[1].version, 2);
    let read = run_sync(store.read("s1", 1)).expect("read");
    assert_eq!(read.len(), 2);
    let latest = run_sync(store.latest_version("s1")).expect("latest");
    assert_eq!(latest, 2);
  }

  #[test]
  fn file_journal_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("events.jsonl");
    let store = FileJournal::<CounterEvt>::open(&path).expect("open");
    run_sync(store.append("acct", &[CounterEvt(10)])).expect("append");
    let read = run_sync(store.read("acct", 1)).expect("read");
    assert_eq!(read[0].payload, CounterEvt(10));
  }

  #[test]
  fn expected_version_conflict() {
    let store = MemoryEventStore::<CounterEvt>::new();
    let first = run_sync(store.append_expected("s", 0, &[CounterEvt(1), CounterEvt(2)]))
      .expect("append at 0");
    assert_eq!(first[1].version, 2);
    let err = run_sync(store.append_expected("s", 1, &[CounterEvt(3)])).expect_err("stale");
    assert_eq!(
      err,
      EventStoreError::VersionConflict {
        stream_id: "s".into(),
        expected: 1,
        actual: 2,
      }
    );
    assert_eq!(run_sync(store.latest_version("s")).expect("latest"), 2);
    let next = run_sync(store.append_expected("s", 2, &[CounterEvt(3)])).expect("append at 2");
    assert_eq!(next[0].version, 3);
  }

  #[test]
  fn append_expected_ahead_of_the_stream_conflicts() {
    let store = MemoryEventStore::<CounterEvt>::new();
    let err = run_sync(store.append_expected("s", 4, &[CounterEvt(1)])).expect_err("gap");
    assert!(matches!(
      err,
      EventStoreError::VersionConflict {
        expected: 4,
        actual: 0,
        ..
      }
    ));
    assert!(run_sync(store.read("s", 1)).expect("read").is_empty());
  }

  #[test]
  fn file_journal_expected_version_conflict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileJournal::<CounterEvt>::open(dir.path().join("ev.jsonl")).expect("open");
    run_sync(store.append_expected("s", 0, &[CounterEvt(1)])).expect("append");
    let err = run_sync(store.append_expected("s", 0, &[CounterEvt(2)])).expect_err("stale");
    assert!(matches!(
      err,
      EventStoreError::VersionConflict {
        expected: 0,
        actual: 1,
        ..
      }
    ));
    assert_eq!(run_sync(store.latest_version("s")).expect("latest"), 1);
  }

  #[test]
  fn arc_store_and_clones_share_streams() {
    let store = MemoryEventStore::<CounterEvt>::new();
    let shared = Arc::new(store.clone());
    run_sync(shared.append("s", &[CounterEvt(1)])).expect("append");
    run_sync(shared.append_expected("s", 1, &[CounterEvt(2)])).expect("append expected");
    assert_eq!(run_sync(store.latest_version("s")).expect("latest"), 2);
    assert_eq!(
      run_sync(shared.read("s", 2)).expect("read")[0].payload,
      CounterEvt(2)
    );
    assert_eq!(run_sync(shared.latest_version("s")).expect("latest"), 2);
  }

  #[test]
  fn memory_default_store_is_empty() {
    let store = MemoryEventStore::<CounterEvt>::default();
    assert_eq!(run_sync(store.latest_version("any")).expect("latest"), 0);
    assert_eq!(run_sync(store.head()).expect("head"), 0);
  }

  #[test]
  fn file_journal_clones_share_the_file_and_an_empty_path_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileJournal::<CounterEvt>::open(dir.path().join("events.jsonl")).expect("open");
    run_sync(store.clone().append("s", &[CounterEvt(1)])).expect("append through a clone");
    assert_eq!(run_sync(store.latest_version("s")).expect("latest"), 1);
    // No parent directory to create, and nothing to open.
    assert!(matches!(
      FileJournal::<CounterEvt>::open(""),
      Err(EventStoreError::Io(_))
    ));
  }

  #[test]
  fn append_empty_returns_no_stored_events() {
    let store = MemoryEventStore::<CounterEvt>::new();
    let stored = run_sync(store.append("s", &[])).expect("append");
    assert!(stored.is_empty());
  }

  #[test]
  fn read_filters_by_from_version() {
    let store = MemoryEventStore::<CounterEvt>::new();
    run_sync(store.append("s", &[CounterEvt(1), CounterEvt(2), CounterEvt(3)])).expect("append");
    let read = run_sync(store.read("s", 2)).expect("read");
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].version, 2);
  }

  #[test]
  fn memory_read_empty_stream_returns_empty() {
    let store = MemoryEventStore::<CounterEvt>::new();
    let read = run_sync(store.read("missing", 1)).expect("read");
    assert!(read.is_empty());
  }

  #[test]
  fn memory_latest_version_zero_when_empty() {
    let store = MemoryEventStore::<CounterEvt>::new();
    let latest = run_sync(store.latest_version("missing")).expect("latest");
    assert_eq!(latest, 0);
  }

  #[test]
  fn file_journal_latest_version_and_read_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested/events.jsonl");
    let store = FileJournal::<CounterEvt>::open(&path).expect("open");
    run_sync(store.append("s", &[CounterEvt(1), CounterEvt(2), CounterEvt(3)])).expect("append");
    assert_eq!(run_sync(store.latest_version("s")).expect("latest"), 3);
    let read = run_sync(store.read("s", 2)).expect("read");
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].version, 2);
  }

  #[test]
  fn file_journal_multiple_streams_and_blank_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("multi.jsonl");
    std::fs::write(
      &path, "

",
    )
    .expect("write blanks");
    let store = FileJournal::<CounterEvt>::open(&path).expect("open");
    run_sync(store.append("a", &[CounterEvt(1)])).expect("a");
    run_sync(store.append("b", &[CounterEvt(2)])).expect("b");
    let a = run_sync(store.read("a", 1)).expect("read a");
    let b = run_sync(store.read("b", 1)).expect("read b");
    assert_eq!(a[0].payload, CounterEvt(1));
    assert_eq!(b[0].payload, CounterEvt(2));
  }

  #[test]
  fn stored_event_clone_and_debug() {
    let evt = StoredEvent {
      event_id: "e1".into(),
      version: 1,
      payload: CounterEvt(7),
    };
    let cloned = evt.clone();
    assert_eq!(format!("{cloned:?}"), format!("{evt:?}"));
  }

  #[test]
  fn file_journal_read_missing_stream_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.jsonl");
    let store = FileJournal::<CounterEvt>::open(&path).expect("open");
    let read = run_sync(store.read("missing", 1)).expect("read");
    assert!(read.is_empty());
    assert_eq!(
      run_sync(store.latest_version("missing")).expect("latest"),
      0
    );
  }

  #[test]
  fn file_journal_rejects_invalid_json_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.jsonl");
    std::fs::write(
      &path,
      "not-json
",
    )
    .expect("write");
    let store = FileJournal::<CounterEvt>::open(&path).expect("open");
    let err = run_sync(store.read("s", 1)).expect_err("serde");
    assert!(matches!(err, EventStoreError::Serde(_)));
  }
}
