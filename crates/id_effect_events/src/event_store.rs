//! [`EventStore`] trait and in-memory / file-backed implementations.

use crate::error::EventStoreError;
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
pub trait EventStore<E>: Send + Sync {
  /// Append events to `stream_id`, returning assigned versions.
  fn append(
    &self,
    stream_id: &str,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError, ()>;

  /// Read events from `stream_id` starting at `from_version` (inclusive, 1-based).
  fn read(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError, ()>;

  /// Latest assigned version, or `0` when the stream is empty.
  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError, ()>;
}

/// In-memory append-only store (tests and ephemeral demos).
pub struct MemoryEventStore<E> {
  streams: Arc<Mutex<HashMap<String, Vec<StoredEvent<E>>>>>,
}

impl<E> MemoryEventStore<E> {
  /// Empty store.
  pub fn new() -> Self {
    Self {
      streams: Arc::new(Mutex::new(HashMap::new())),
    }
  }
}

impl<E> Default for MemoryEventStore<E> {
  fn default() -> Self {
    Self::new()
  }
}

impl<E: Clone + Send + Sync + 'static> EventStore<E> for MemoryEventStore<E> {
  fn append(
    &self,
    stream_id: &str,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let events: Vec<E> = events.to_vec();
    let streams = Arc::clone(&self.streams);
    Effect::new(move |_r| {
      let mut guard = streams
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      let stream = guard.entry(stream_id).or_default();
      let mut out = Vec::with_capacity(events.len());
      for payload in events {
        let version = stream.len() as u64 + 1;
        let stored = StoredEvent {
          event_id: Uuid::new_v4().to_string(),
          version,
          payload,
        };
        stream.push(stored.clone());
        out.push(stored);
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
    let streams = Arc::clone(&self.streams);
    Effect::new(move |_r| {
      let guard = streams
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      let Some(stream) = guard.get(&stream_id) else {
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

  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let streams = Arc::clone(&self.streams);
    Effect::new(move |_r| {
      let guard = streams
        .lock()
        .map_err(|e| EventStoreError::Io(e.to_string()))?;
      Ok(guard.get(&stream_id).map(|s| s.len() as u64).unwrap_or(0))
    })
  }
}

/// JSON-lines journal on disk (one [`StoredEvent`] per line).
pub struct FileJournal<E> {
  path: PathBuf,
  _marker: std::marker::PhantomData<E>,
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

impl<E: Clone + Serialize + DeserializeOwned + Send + Sync + 'static> EventStore<E>
  for FileJournal<E>
{
  fn append(
    &self,
    stream_id: &str,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let events: Vec<E> = events.to_vec();
    let path = self.path.clone();
    Effect::new(move |_r| {
      let journal = FileJournal::<E> {
        path,
        _marker: std::marker::PhantomData,
      };
      let mut streams = journal.read_all()?;
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

  fn read(
    &self,
    stream_id: &str,
    from_version: u64,
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let path = self.path.clone();
    Effect::new(move |_r| {
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

  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError, ()> {
    let stream_id = stream_id.to_owned();
    let path = self.path.clone();
    Effect::new(move |_r| {
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
  use id_effect::run_blocking;

  #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
  struct CounterEvt(u32);

  #[test]
  fn memory_store_appends_and_reads() {
    let store = MemoryEventStore::<CounterEvt>::new();
    let stored =
      run_blocking(store.append("s1", &[CounterEvt(1), CounterEvt(2)]), ()).expect("append");
    assert_eq!(stored[0].version, 1);
    assert_eq!(stored[1].version, 2);
    let read = run_blocking(store.read("s1", 1), ()).expect("read");
    assert_eq!(read.len(), 2);
    let latest = run_blocking(store.latest_version("s1"), ()).expect("latest");
    assert_eq!(latest, 2);
  }

  #[test]
  fn file_journal_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("events.jsonl");
    let store = FileJournal::<CounterEvt>::open(&path).expect("open");
    run_blocking(store.append("acct", &[CounterEvt(10)]), ()).expect("append");
    let read = run_blocking(store.read("acct", 1), ()).expect("read");
    assert_eq!(read[0].payload, CounterEvt(10));
  }
}
