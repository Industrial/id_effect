//! SQL journal backend and event store tests.

use std::sync::Arc;

use id_effect_events::{EventStore, SqlEventJournal, SqlJournalBackend, TestSqlJournalBackend};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Ping {
  n: u32,
}

#[test]
fn test_backend_round_trip() {
  let backend = TestSqlJournalBackend::default();
  backend
    .insert_row("s1", 1, "e1", r#"{"n":1}"#)
    .expect("insert");
  let rows = backend.select_from("s1", 1).expect("select");
  assert_eq!(rows.len(), 1);
  assert_eq!(rows[0].0, 1);
}

#[test]
fn test_backend_version_conflict() {
  let backend = TestSqlJournalBackend::default();
  backend.insert_row("s", 1, "a", "{}").unwrap();
  let err = backend.insert_row("s", 1, "b", "{}").unwrap_err();
  assert!(matches!(
    err,
    id_effect_events::EventStoreError::VersionConflict { .. }
  ));
}

#[test]
fn sql_event_journal_append_and_read() {
  let journal = SqlEventJournal::<Ping>::new(Arc::new(TestSqlJournalBackend::default()));
  let appended = journal
    .append("stream", &[Ping { n: 7 }, Ping { n: 8 }])
    .block_on()
    .into_result()
    .expect("append");
  assert_eq!(appended.len(), 2);
  assert_eq!(appended[0].version, 1);
  assert_eq!(appended[1].version, 2);
  let read = journal
    .read("stream", 1)
    .block_on()
    .into_result()
    .expect("read");
  assert_eq!(read.len(), 2);
  assert_eq!(read[0].payload, Ping { n: 7 });
  let latest = journal
    .latest_version("stream")
    .block_on()
    .into_result()
    .expect("latest");
  assert_eq!(latest, 2);
}
