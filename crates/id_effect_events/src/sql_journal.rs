//! SQL event journal over [`id_effect_sql`]'s [`SqlClient`] (PostgreSQL and SQLite).
//!
//! One table, [`SQL_EVENT_JOURNAL_DDL`], keyed by `(stream_id, version)`: the primary key is the
//! optimistic-concurrency guard. An append runs in one transaction that reads the stream's
//! version, checks it against the expected one and inserts the events at the following
//! versions; a concurrent writer that took one of those versions first makes the insert hit the
//! key ([`SqlError::is_unique_violation`]), which becomes [`EventStoreError::VersionConflict`]
//! and rolls the whole append back.

use crate::error::EventStoreError;
use crate::event_store::{EventStore, StoredEvent, check_version};
use id_effect::{Effect, fail, succeed};
use id_effect_sql::{SqlClient, SqlClientService, SqlError, SqlRow, params, with_transaction};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::Arc;
use uuid::Uuid;

/// DDL of the journal table (portable PostgreSQL / SQLite; payloads are JSON text). Apply it in
/// a migration, or at startup with [`SqlEventJournal::create_table`].
pub const SQL_EVENT_JOURNAL_DDL: &str = "CREATE TABLE IF NOT EXISTS id_effect_event_journal (
  stream_id TEXT NOT NULL,
  version BIGINT NOT NULL,
  event_id TEXT NOT NULL,
  payload TEXT NOT NULL,
  PRIMARY KEY (stream_id, version)
)";

const LATEST_SQL: &str = "SELECT COALESCE(MAX(version), 0) AS latest \
  FROM id_effect_event_journal WHERE stream_id = $1";

const INSERT_SQL: &str = "INSERT INTO id_effect_event_journal \
  (stream_id, version, event_id, payload) VALUES ($1, $2, $3, $4)";

const SELECT_SQL: &str = "SELECT version, event_id, payload FROM id_effect_event_journal \
  WHERE stream_id = $1 AND version >= $2 ORDER BY version";

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

fn to_version(raw: i64) -> Result<u64, EventStoreError> {
  u64::try_from(raw).map_err(|_| EventStoreError::Io(format!("negative stream version {raw}")))
}

fn to_sql_version(version: u64) -> Result<i64, EventStoreError> {
  i64::try_from(version)
    .map_err(|_| EventStoreError::Io(format!("stream version {version} exceeds i64")))
}

fn latest_from(rows: &[SqlRow]) -> Result<u64, EventStoreError> {
  match rows.first() {
    Some(row) => to_version(row.get::<i64>("latest")?),
    None => Ok(0),
  }
}

fn ready<A: Send + 'static, E: Send + 'static>(r: Result<A, E>) -> Effect<A, E> {
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
    let encoded = match encoded {
      Ok(encoded) => encoded,
      Err(e) => return fail(e),
    };
    let stream_id = stream_id.to_owned();
    let conflict = (Arc::clone(&self.client), stream_id.clone());
    with_transaction(Arc::clone(&self.client), move |tx| {
      tx.query(LATEST_SQL, &params![stream_id.as_str()])
        .map_err(AppendError::from)
        .flat_map(move |rows| {
          let current = match latest_from(&rows)
            .and_then(|current| check_version(&stream_id, expected, current).map(|()| current))
          {
            Ok(current) => current,
            Err(e) => return fail(AppendError::Store(e)),
          };
          let mut inserts: Effect<(), AppendError> = succeed(());
          let mut out = Vec::with_capacity(encoded.len());
          for (i, (event_id, json, payload)) in encoded.into_iter().enumerate() {
            let version = current + i as u64 + 1;
            let sql_version = match to_sql_version(version) {
              Ok(v) => v,
              Err(e) => return fail(AppendError::Store(e)),
            };
            let insert = tx
              .execute(
                INSERT_SQL,
                &params![stream_id.as_str(), sql_version, event_id.as_str(), json],
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
    .catch(move |e| match e {
      AppendError::Store(e) => fail(e),
      AppendError::Raced { expected } => {
        let (client, stream_id) = conflict;
        latest(&client, &stream_id).flat_map(move |actual| {
          fail(EventStoreError::VersionConflict {
            stream_id,
            expected,
            actual,
          })
        })
      }
    })
  }
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
      .flat_map(|rows| {
        ready(
          rows
            .iter()
            .map(|row| {
              let json = row.get::<String>("payload")?;
              Ok(StoredEvent {
                event_id: row.get::<String>("event_id")?,
                version: to_version(row.get::<i64>("version")?)?,
                payload: serde_json::from_str(&json)
                  .map_err(|e| EventStoreError::Serde(e.to_string()))?,
              })
            })
            .collect::<Result<Vec<_>, EventStoreError>>(),
        )
      })
  }

  fn latest_version(&self, stream_id: &str) -> Effect<u64, EventStoreError> {
    latest(&self.client, stream_id)
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
    assert_eq!(inserts[1].1[1], id_effect_sql::SqlValue::I64(2));
    assert_eq!(
      inserts[0].1[3],
      id_effect_sql::SqlValue::Text(r#"{"n":1}"#.into())
    );
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
