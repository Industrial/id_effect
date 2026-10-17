//! SQL event journal over [`id_effect_sql`]'s [`SqlClient`] (PostgreSQL and SQLite).
//!
//! One table, [`SQL_EVENT_JOURNAL_DDL`], keyed by `(stream_id, version)`: the primary key is the
//! optimistic-concurrency guard. An append runs in one transaction that reads the stream's
//! version, checks it against the expected one and inserts the events at the following
//! versions; a concurrent writer that took one of those versions first makes the insert hit the
//! key ([`SqlError::is_unique_violation`]), which becomes [`EventStoreError::VersionConflict`]
//! and rolls the whole append back.
//!
//! Every row also gets a global `position` (unique, `MAX(position) + 1` inside the append's
//! transaction), the order [`EventLog::read_all`] and catch-up subscriptions read in. A
//! transaction that takes a position blocks the next writer's insert of the same position until
//! it commits, so positions are gap-free and become visible in order: a reader that has seen
//! position `n` never sees a smaller one appear later. The cost is that appends are serialized
//! on the journal; a writer that loses the position to another stream's append is retried (up to
//! [`POSITION_RETRIES`] times) rather than reported as a conflict.

use crate::error::EventStoreError;
use crate::event_store::{EventStore, StoredEvent, check_version};
use crate::subscription::{EventLog, RecordedEvent};
use id_effect::{Effect, fail, succeed};
use id_effect_sql::{SqlClient, SqlClientService, SqlError, SqlRow, params, with_transaction};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::Arc;
use uuid::Uuid;

/// DDL of the journal table (portable PostgreSQL / SQLite; payloads are JSON text). Apply it in
/// a migration, or at startup with [`SqlEventJournal::create_table`].
///
/// `position` is the global order across streams (see the module docs); `(stream_id, version)`
/// is the per-stream order and the optimistic-concurrency key.
pub const SQL_EVENT_JOURNAL_DDL: &str = "CREATE TABLE IF NOT EXISTS id_effect_event_journal (
  position BIGINT NOT NULL UNIQUE,
  stream_id TEXT NOT NULL,
  version BIGINT NOT NULL,
  event_id TEXT NOT NULL,
  payload TEXT NOT NULL,
  PRIMARY KEY (stream_id, version)
)";

const LATEST_SQL: &str = "SELECT COALESCE(MAX(version), 0) AS latest \
  FROM id_effect_event_journal WHERE stream_id = $1";

const HEAD_SQL: &str = "SELECT COALESCE(MAX(position), 0) AS head FROM id_effect_event_journal";

const INSERT_SQL: &str = "INSERT INTO id_effect_event_journal \
  (position, stream_id, version, event_id, payload) VALUES ($1, $2, $3, $4, $5)";

const SELECT_SQL: &str = "SELECT version, event_id, payload FROM id_effect_event_journal \
  WHERE stream_id = $1 AND version >= $2 ORDER BY version";

const READ_ALL_SQL: &str = "SELECT position, stream_id, version, event_id, payload \
  FROM id_effect_event_journal WHERE position > $1 ORDER BY position LIMIT $2";

/// How many times an append that lost the global position to another stream's append is
/// retried before it fails with [`EventStoreError::Io`].
pub const POSITION_RETRIES: u32 = 32;

/// [`EventStore`] persisted through a [`SqlClient`] (`PgSqlClient`, `SqliteSqlClient`, …) in
/// the table of [`SQL_EVENT_JOURNAL_DDL`]. Payloads are stored as JSON. Clones share the client.
pub struct SqlEventJournal<E> {
  client: Arc<dyn SqlClient>,
  _marker: std::marker::PhantomData<fn() -> E>,
}

impl<E> Clone for SqlEventJournal<E> {
  fn clone(&self) -> Self {
    Self {
      client: Arc::clone(&self.client),
      _marker: std::marker::PhantomData,
    }
  }
}

/// How an append inside the transaction failed: a store error, or a concurrent writer that took
/// a version first (the conflict's `actual` is read after the rollback).
enum AppendError {
  Store(EventStoreError),
  Raced { expected: u64 },
}

impl From<SqlError> for AppendError {
  fn from(e: SqlError) -> Self {
    AppendError::Store(EventStoreError::Sql(e))
  }
}

pub(crate) fn to_version(raw: i64) -> Result<u64, EventStoreError> {
  u64::try_from(raw).map_err(|_| EventStoreError::Io(format!("negative stream version {raw}")))
}

pub(crate) fn to_sql_version(version: u64) -> Result<i64, EventStoreError> {
  i64::try_from(version)
    .map_err(|_| EventStoreError::Io(format!("stream version {version} exceeds i64")))
}

/// The `u64` in column `column` of the first row (`0` without rows).
fn first_u64(rows: &[SqlRow], column: &str) -> Result<u64, EventStoreError> {
  match rows.first() {
    Some(row) => to_version(row.get::<i64>(column)?),
    None => Ok(0),
  }
}

fn latest_from(rows: &[SqlRow]) -> Result<u64, EventStoreError> {
  first_u64(rows, "latest")
}

/// A journal row's event (`version`, `event_id`, JSON `payload`).
fn decode_event<E: DeserializeOwned>(row: &SqlRow) -> Result<StoredEvent<E>, EventStoreError> {
  let json = row.get::<String>("payload")?;
  Ok(StoredEvent {
    event_id: row.get::<String>("event_id")?,
    version: to_version(row.get::<i64>("version")?)?,
    payload: serde_json::from_str(&json).map_err(|e| EventStoreError::Serde(e.to_string()))?,
  })
}

pub(crate) fn ready<A: Send + 'static, E: Send + 'static>(r: Result<A, E>) -> Effect<A, E> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

fn latest(client: &Arc<dyn SqlClient>, stream_id: &str) -> Effect<u64, EventStoreError> {
  client
    .query(LATEST_SQL, &params![stream_id])
    .map_err(EventStoreError::from)
    .flat_map(|rows| ready(latest_from(&rows)))
}

impl<E> SqlEventJournal<E> {
  /// A journal over `client`.
  pub fn new(client: Arc<dyn SqlClient>) -> Self {
    Self {
      client,
      _marker: std::marker::PhantomData,
    }
  }

  /// A journal over the client of a [`SqlClientService`] (e.g. one read with
  /// `need::<SqlClientService>()`).
  pub fn from_service(service: &SqlClientService) -> Self {
    Self::new(Arc::clone(&service.0))
  }

  /// The client the journal runs on.
  pub fn client(&self) -> &Arc<dyn SqlClient> {
    &self.client
  }

  /// Run [`SQL_EVENT_JOURNAL_DDL`] (idempotent).
  pub fn create_table(&self) -> Effect<(), EventStoreError> {
    self
      .client
      .execute(SQL_EVENT_JOURNAL_DDL, &[])
      .map(|_| ())
      .map_err(EventStoreError::from)
  }
}

impl<E: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> SqlEventJournal<E> {
  fn append_at(
    &self,
    stream_id: &str,
    expected: Option<u64>,
    events: &[E],
  ) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
    let encoded: Result<Vec<(String, String, E)>, EventStoreError> = events
      .iter()
      .map(|payload| {
        let json =
          serde_json::to_string(payload).map_err(|e| EventStoreError::Serde(e.to_string()))?;
        Ok((Uuid::new_v4().to_string(), json, payload.clone()))
      })
      .collect();
    match encoded {
      Ok(encoded) => append_attempt(
        Arc::clone(&self.client),
        stream_id.to_owned(),
        expected,
        encoded,
        POSITION_RETRIES,
      ),
      Err(e) => fail(e),
    }
  }
}

/// One transaction of an append: version check, global head, inserts. A unique violation
/// rolls it back; if the stream did not move meanwhile, another stream's append took the
/// position and the append is retried (`retries` left), else it is a version conflict.
fn append_attempt<E: Clone + Send + Sync + 'static>(
  client: Arc<dyn SqlClient>,
  stream_id: String,
  expected: Option<u64>,
  encoded: Vec<(String, String, E)>,
  retries: u32,
) -> Effect<Vec<StoredEvent<E>>, EventStoreError> {
  let again = (Arc::clone(&client), stream_id.clone(), encoded.clone());
  with_transaction(client, move |tx| {
    tx.query(LATEST_SQL, &params![stream_id.as_str()])
      .map_err(AppendError::from)
      .flat_map(move |rows| {
        let current = match latest_from(&rows)
          .and_then(|current| check_version(&stream_id, expected, current).map(|()| current))
        {
          Ok(current) => current,
          Err(e) => return fail(AppendError::Store(e)),
        };
        tx.query(HEAD_SQL, &[])
          .map_err(AppendError::from)
          .flat_map(move |rows| {
            let head = match first_u64(&rows, "head") {
              Ok(head) => head,
              Err(e) => return fail(AppendError::Store(e)),
            };
            let mut inserts: Effect<(), AppendError> = succeed(());
            let mut out = Vec::with_capacity(encoded.len());
            for (i, (event_id, json, payload)) in encoded.into_iter().enumerate() {
              let version = current + i as u64 + 1;
              let keys = to_sql_version(head + i as u64 + 1)
                .and_then(|position| Ok((position, to_sql_version(version)?)));
              let (sql_position, sql_version) = match keys {
                Ok(keys) => keys,
                Err(e) => return fail(AppendError::Store(e)),
              };
              let insert = tx
                .execute(
                  INSERT_SQL,
                  &params![
                    sql_position,
                    stream_id.as_str(),
                    sql_version,
                    event_id.as_str(),
                    json
                  ],
                )
                .map(|_| ())
                .map_err(move |e| {
                  if e.is_unique_violation() {
                    AppendError::Raced { expected: current }
                  } else {
                    AppendError::from(e)
                  }
                });
              inserts = inserts.then(insert);
              out.push(StoredEvent {
                event_id,
                version,
                payload,
              });
            }
            inserts.map(move |()| out)
          })
      })
  })
  .catch(move |e| match e {
    AppendError::Store(e) => fail(e),
    AppendError::Raced { expected: current } => {
      let (client, stream_id, encoded) = again;
      latest(&client, &stream_id).flat_map(move |actual| {
        if actual != current {
          fail(EventStoreError::VersionConflict {
            stream_id,
            expected: current,
            actual,
          })
        } else if retries > 0 {
          append_attempt(client, stream_id, expected, encoded, retries - 1)
        } else {
          fail(EventStoreError::Io(format!(
            "append to `{stream_id}` lost the global position {} times",
            POSITION_RETRIES + 1
          )))
        }
      })
    }
  })
}

impl<E: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> EventStore<E>
  for SqlEventJournal<E>
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
    let from = match to_sql_version(from_version) {
      Ok(v) => v,
      Err(e) => return fail(e),
    };
    self
      .client
      .query(SELECT_SQL, &params![stream_id, from])
      .map_err(EventStoreError::from)
      .flat_map(|rows| ready(rows.iter().map(decode_event).collect()))
  }

  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError> {
    latest(&self.client, stream_id)
  }
}

impl<E: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> EventLog<E>
  for SqlEventJournal<E>
{
  fn read_all(&self, after: u64, limit: usize) -> Effect<Vec<RecordedEvent<E>>, EventStoreError> {
    let after = match to_sql_version(after) {
      Ok(v) => v,
      Err(e) => return fail(e),
    };
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    self
      .client
      .query(READ_ALL_SQL, &params![after, limit])
      .map_err(EventStoreError::from)
      .flat_map(|rows| {
        ready(
          rows
            .iter()
            .map(|row| {
              Ok(RecordedEvent {
                position: to_version(row.get::<i64>("position")?)?,
                stream_id: row.get::<String>("stream_id")?,
                event: decode_event(row)?,
              })
            })
            .collect(),
        )
      })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::edge::run_sync;
  use id_effect_sql::TestSqlClient;

  #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  struct Ping {
    n: u32,
  }

  fn connected() -> TestSqlClient {
    let test = TestSqlClient::new();
    run_sync(test.connect()).expect("connect");
    test
  }

  #[test]
  fn create_table_runs_the_ddl() {
    let test = connected();
    let journal = SqlEventJournal::<Ping>::new(Arc::new(test.clone()));
    run_sync(journal.create_table()).expect("ddl");
    assert_eq!(test.recorded_executes()[0].0, SQL_EVENT_JOURNAL_DDL);
    assert!(Arc::ptr_eq(journal.client(), journal.clone().client()));
  }

  #[test]
  fn append_inserts_rows_in_one_transaction() {
    let test = connected();
    let journal = SqlEventJournal::<Ping>::from_service(&SqlClientService::new(test.clone()));
    let stored =
      run_sync(journal.append_expected("s", 0, &[Ping { n: 1 }, Ping { n: 2 }])).expect("append");
    assert_eq!(stored.iter().map(|s| s.version).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(test.commits(), 1);
    let inserts = test.recorded_executes();
    assert_eq!(inserts.len(), 2);
    // (position, stream_id, version, event_id, payload): the journal head is 0 (no rows).
    assert_eq!(inserts[1].1[0], id_effect_sql::SqlValue::I64(2));
    assert_eq!(inserts[1].1[2], id_effect_sql::SqlValue::I64(2));
    assert_eq!(
      inserts[0].1[4],
      id_effect_sql::SqlValue::Text(r#"{"n":1}"#.into())
    );
  }

  #[test]
  fn append_takes_positions_after_the_journal_head() {
    let test = connected();
    test.script_query(HEAD_SQL, vec![SqlRow::from_pairs([("head", 41i64)])]);
    let journal = SqlEventJournal::<Ping>::new(Arc::new(test.clone()));
    run_sync(journal.append("s", &[Ping { n: 1 }, Ping { n: 2 }])).expect("append");
    let positions: Vec<_> = test
      .recorded_executes()
      .iter()
      .map(|(_, params)| params[0].clone())
      .collect();
    assert_eq!(
      positions,
      [
        id_effect_sql::SqlValue::I64(42),
        id_effect_sql::SqlValue::I64(43)
      ]
    );
  }

  #[test]
  fn read_all_decodes_positions_and_streams() {
    let test = connected();
    test.script_query(
      READ_ALL_SQL,
      vec![SqlRow::from_pairs([
        ("position", id_effect_sql::SqlValue::I64(7)),
        ("stream_id", id_effect_sql::SqlValue::Text("a".into())),
        ("version", id_effect_sql::SqlValue::I64(2)),
        ("event_id", id_effect_sql::SqlValue::Text("e7".into())),
        (
          "payload",
          id_effect_sql::SqlValue::Text(r#"{"n":3}"#.into()),
        ),
      ])],
    );
    let journal = SqlEventJournal::<Ping>::new(Arc::new(test.clone()));
    let all = run_sync(journal.read_all(6, 10)).expect("read_all");
    assert_eq!(
      (
        all[0].position,
        all[0].stream_id.as_str(),
        all[0].event.version
      ),
      (7, "a", 2)
    );
    assert_eq!(all[0].event.payload, Ping { n: 3 });
    let (_, params) = test.recorded_queries().pop().expect("query");
    assert_eq!(
      params,
      [
        id_effect_sql::SqlValue::I64(6),
        id_effect_sql::SqlValue::I64(10)
      ]
    );
    assert!(matches!(
      run_sync(journal.read_all(u64::MAX, 1)).expect_err("range"),
      EventStoreError::Io(_)
    ));
  }

  #[test]
  fn stale_expected_version_rolls_back() {
    let test = connected();
    test.script_query(LATEST_SQL, vec![SqlRow::from_pairs([("latest", 3i64)])]);
    let journal = SqlEventJournal::<Ping>::new(Arc::new(test.clone()));
    let err = run_sync(journal.append_expected("s", 1, &[Ping { n: 1 }])).expect_err("stale");
    assert_eq!(
      err,
      EventStoreError::VersionConflict {
        stream_id: "s".into(),
        expected: 1,
        actual: 3,
      }
    );
    assert_eq!((test.commits(), test.rollbacks()), (0, 1));
    assert_eq!(run_sync(journal.latest_version("s")).expect("latest"), 3);
  }

  #[test]
  fn read_decodes_rows_and_reports_bad_payloads() {
    let test = connected();
    test.script_query(
      SELECT_SQL,
      vec![SqlRow::from_pairs([
        ("version", id_effect_sql::SqlValue::I64(1)),
        ("event_id", id_effect_sql::SqlValue::Text("e1".into())),
        (
          "payload",
          id_effect_sql::SqlValue::Text(r#"{"n":7}"#.into()),
        ),
      ])],
    );
    let journal = SqlEventJournal::<Ping>::new(Arc::new(test.clone()));
    let read = run_sync(journal.read("s", 1)).expect("read");
    assert_eq!(read[0].payload, Ping { n: 7 });
    assert_eq!(read[0].event_id, "e1");

    test.script_query(
      SELECT_SQL,
      vec![SqlRow::from_pairs([
        ("version", id_effect_sql::SqlValue::I64(1)),
        ("event_id", id_effect_sql::SqlValue::Text("e1".into())),
        ("payload", id_effect_sql::SqlValue::Text("nope".into())),
      ])],
    );
    let err = run_sync(journal.read("s", 1)).expect_err("serde");
    assert!(matches!(err, EventStoreError::Serde(_)));
  }

  #[test]
  fn sql_failures_and_out_of_range_versions_are_typed() {
    let journal = SqlEventJournal::<Ping>::new(Arc::new(TestSqlClient::new()));
    assert_eq!(
      run_sync(journal.latest_version("s")).expect_err("not connected"),
      EventStoreError::Sql(SqlError::NotConnected)
    );
    assert!(matches!(
      run_sync(journal.read("s", u64::MAX)).expect_err("range"),
      EventStoreError::Io(_)
    ));
    assert!(to_version(-1).is_err());
    assert_eq!(latest_from(&[]).expect("empty"), 0);
  }
}
