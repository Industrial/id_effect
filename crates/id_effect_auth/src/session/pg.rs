//! [`PgSessionStore`]: a [`SessionStore`] in PostgreSQL over `id_effect_sql`'s [`SqlClient`]
//! (feature `pg`).

use std::collections::BTreeMap;
use std::fmt;

use id_effect::{Effect, Layer, UtcDateTime, fail, hlist, need, succeed};
use id_effect_sql::{Json, SqlClient, SqlClientEnv, SqlClientService, SqlError, SqlRow, params};
use sha2::{Digest, Sha256};

use super::store::{SessionStore, SessionStoreEnv, SessionStoreService};
use super::{Session, SessionError, SessionId};

/// The crate's migration `migrations/0001_id_effect_sessions.up.sql`: what
/// [`PgSessionStore::ensure_schema`] runs.
const SCHEMA: &str = include_str!("../../migrations/0001_id_effect_sessions.up.sql");

const UPSERT: &str = "INSERT INTO id_effect_sessions \
  (id_hash, user_id, attrs, created_at, last_seen_at, idle_expires_at, absolute_expires_at) \
  VALUES ($1, $2, $3, $4, $5, $6, $7) \
  ON CONFLICT (id_hash) DO UPDATE SET user_id = EXCLUDED.user_id, attrs = EXCLUDED.attrs, \
  created_at = EXCLUDED.created_at, last_seen_at = EXCLUDED.last_seen_at, \
  idle_expires_at = EXCLUDED.idle_expires_at, absolute_expires_at = EXCLUDED.absolute_expires_at";

const LOAD: &str = "SELECT user_id, attrs, created_at, last_seen_at, idle_expires_at, \
  absolute_expires_at FROM id_effect_sessions WHERE id_hash = $1";

const REMOVE: &str = "DELETE FROM id_effect_sessions WHERE id_hash = $1";

const REMOVE_USER: &str = "DELETE FROM id_effect_sessions WHERE user_id = $1";

const PURGE: &str =
  "DELETE FROM id_effect_sessions WHERE idle_expires_at <= $1 OR absolute_expires_at <= $1";

/// [`SessionStore`] in the PostgreSQL table `id_effect_sessions`, over any [`SqlClient`]
/// (`id_effect_sql::pg::PgSqlClient` in production). Clones share the client.
///
/// - The primary key is the lowercase hex SHA-256 of the session id; the id itself (the cookie
///   value) is never written, so a leaked table or backup yields no usable session.
/// - Attributes are a `jsonb` object of strings; the four times are `timestamptz`, stored to the
///   microsecond (finer digits are truncated).
/// - [`save`](SessionStore::save) is one upsert (`INSERT … ON CONFLICT (id_hash) DO UPDATE`).
/// - [`purge_expired`](SessionStore::purge_expired) deletes the records whose idle or absolute
///   expiry is at or before `now`.
///
/// Create the table with the crate's `migrations/0001_id_effect_sessions.up.sql` (an
/// `id_effect_sql` migration) or with [`ensure_schema`](Self::ensure_schema), which runs the same
/// statements idempotently. Driver and decode failures surface as [`SessionError::Storage`].
#[derive(Clone)]
pub struct PgSessionStore {
  client: SqlClientService,
}

impl fmt::Debug for PgSessionStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("PgSessionStore")
      .field("table", &"id_effect_sessions")
      .finish_non_exhaustive()
  }
}

impl PgSessionStore {
  /// A store over `client`.
  #[inline]
  pub fn new(client: impl SqlClient) -> Self {
    Self::from_service(SqlClientService::new(client))
  }

  /// A store over the client capability `client` (shares the client and its pool).
  #[inline]
  pub fn from_service(client: SqlClientService) -> Self {
    Self { client }
  }

  /// Create `id_effect_sessions` and its indexes unless they exist: the statements of
  /// `migrations/0001_id_effect_sessions.up.sql`, one at a time. Safe to run at every start;
  /// two instances starting at the same moment may race on `CREATE TABLE` (run the migration
  /// instead when that matters).
  pub fn ensure_schema(&self) -> Effect<(), SessionError> {
    let client = self.client.clone();
    statements(SCHEMA)
      .into_iter()
      .fold(succeed(()), move |done, sql| {
        let client = client.clone();
        done.flat_map(move |()| client.execute(&sql, &[]).map(|_| ()).map_err(storage))
      })
  }
}

/// The SQL statements of a migration file: `--` comment lines dropped, split at `;`.
fn statements(script: &str) -> Vec<String> {
  let code: String = script
    .lines()
    .filter(|line| !line.trim_start().starts_with("--"))
    .map(|line| format!("{line}\n"))
    .collect();
  code
    .split(';')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_owned)
    .collect()
}

/// The table's key for `id`: lowercase hex SHA-256 of the id.
fn id_hash(id: &SessionId) -> String {
  Sha256::digest(id.as_str().as_bytes())
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}

fn storage(e: SqlError) -> SessionError {
  SessionError::Storage(e.to_string())
}

fn attrs_json(attrs: &BTreeMap<String, String>) -> serde_json::Value {
  serde_json::Value::Object(
    attrs
      .iter()
      .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
      .collect(),
  )
}

/// The session `id` from its `LOAD` row.
fn decode(id: SessionId, row: &SqlRow) -> Result<Session, SqlError> {
  let at = |column: &str| row.get::<jiff::Timestamp>(column).map(UtcDateTime);
  let Json(attrs) = row.get::<Json<BTreeMap<String, String>>>("attrs")?;
  Ok(Session::from_parts(
    id,
    row.get::<String>("user_id")?,
    attrs,
    at("created_at")?,
    at("last_seen_at")?,
    at("idle_expires_at")?,
    at("absolute_expires_at")?,
  ))
}

impl SessionStore for PgSessionStore {
  fn save(&self, session: Session) -> Effect<(), SessionError> {
    let params = params![
      id_hash(&session.id),
      session.user_id,
      attrs_json(&session.attrs),
      session.created_at.0,
      session.last_seen_at.0,
      session.idle_expires_at.0,
      session.absolute_expires_at.0,
    ];
    self
      .client
      .execute(UPSERT, &params)
      .map(|_| ())
      .map_err(storage)
  }

  fn load(&self, id: &SessionId) -> Effect<Option<Session>, SessionError> {
    let id = id.clone();
    self
      .client
      .query(LOAD, &params![id_hash(&id)])
      .map_err(storage)
      .flat_map(move |rows| match rows.first().map(|row| decode(id, row)) {
        None => succeed(None),
        Some(Ok(session)) => succeed(Some(session)),
        Some(Err(e)) => fail(storage(e)),
      })
  }

  fn remove(&self, id: &SessionId) -> Effect<bool, SessionError> {
    self
      .client
      .execute(REMOVE, &params![id_hash(id)])
      .map(|n| n > 0)
      .map_err(storage)
  }

  fn remove_user(&self, user_id: &str) -> Effect<u64, SessionError> {
    self
      .client
      .execute(REMOVE_USER, &params![user_id])
      .map_err(storage)
  }

  fn purge_expired(&self, now: UtcDateTime) -> Effect<u64, SessionError> {
    self.client.execute(PURGE, &params![now.0]).map_err(storage)
  }
}

/// A [`Layer`] providing a [`PgSessionStore`] as the [`SessionStoreService`], over the
/// [`SqlClientService`] it needs (feed it `PgSqlClient::layer(config)` with `provide_to`). It
/// does not create the table: apply the migration or run
/// [`PgSessionStore::ensure_schema`] at startup.
pub fn pg_session_store_layer<E: Send + 'static>() -> Layer<SessionStoreEnv, E, SqlClientEnv> {
  Layer::effect(|| {
    need::<SqlClientService>().map(|client| {
      hlist![SessionStoreService::new(PgSessionStore::from_service(
        (*client).clone()
      ))]
    })
  })
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};
  use id_effect_sql::{SqlValue, TestSqlClient, provide_sql_client};

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, SessionError>) -> Result<A, SessionError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn at(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn connected() -> (PgSessionStore, TestSqlClient) {
    let client = TestSqlClient::new();
    run(client.connect().map_err(storage)).unwrap();
    (PgSessionStore::new(client.clone()), client)
  }

  fn session() -> Session {
    let mut s = Session::from_parts(
      SessionId::generate().unwrap(),
      "u",
      BTreeMap::new(),
      at(1),
      at(2),
      at(3),
      at(4),
    );
    s.set_attr("tenant", "acme");
    s
  }

  fn row(s: &Session) -> SqlRow {
    SqlRow::from_pairs([
      ("user_id", SqlValue::Text(s.user_id().into())),
      ("attrs", SqlValue::Json(attrs_json(s.attrs()))),
      ("created_at", SqlValue::Timestamp(s.created_at().0)),
      ("last_seen_at", SqlValue::Timestamp(s.last_seen_at().0)),
      (
        "idle_expires_at",
        SqlValue::Timestamp(s.idle_expires_at().0),
      ),
      (
        "absolute_expires_at",
        SqlValue::Timestamp(s.absolute_expires_at().0),
      ),
    ])
  }

  #[test]
  fn the_key_is_the_sha256_of_the_id_never_the_id() {
    let id = SessionId::parse(&"0".repeat(64)).unwrap();
    // echo -n 000…0 (64 zeros) | sha256sum
    assert_eq!(
      id_hash(&id),
      "60e05bd1b195af2f94112fa7197a5c88289058840ce7c6df9693756bc6250f55"
    );
    let (store, client) = connected();
    let s = session();
    run(store.save(s.clone())).unwrap();
    let (sql, params) = &client.recorded_executes()[0];
    assert_eq!(sql, UPSERT);
    assert_eq!(params[0], SqlValue::Text(id_hash(s.id())));
    assert!(
      params
        .iter()
        .all(|p| *p != SqlValue::Text(s.id().as_str().into())),
      "the raw id is never bound"
    );
    assert_eq!(
      params[2],
      SqlValue::Json(serde_json::json!({"tenant": "acme"}))
    );
    assert_eq!(params[6], SqlValue::Timestamp(at(4).0));
  }

  #[test]
  fn load_decodes_the_row_under_the_presented_id() {
    let (store, client) = connected();
    let s = session();
    client.script_query(LOAD, vec![row(&s)]);
    assert_eq!(run(store.load(s.id())), Ok(Some(s.clone())));
    assert_eq!(
      client.recorded_queries()[0].1,
      [SqlValue::Text(id_hash(s.id()))]
    );
    client.script_query(LOAD, vec![]);
    assert_eq!(run(store.load(s.id())), Ok(None));
  }

  #[test]
  fn a_malformed_row_is_a_storage_error() {
    let (store, client) = connected();
    let s = session();
    let bad = SqlRow::from_pairs([("user_id", SqlValue::I64(1))]);
    client.script_query(LOAD, vec![bad]);
    let Err(SessionError::Storage(m)) = run(store.load(s.id())) else {
      panic!("expected a storage error")
    };
    assert!(m.contains("attrs"), "{m}");
  }

  #[test]
  fn deletes_report_the_affected_rows() {
    let (store, client) = connected();
    let id = SessionId::generate().unwrap();
    assert_eq!(run(store.remove(&id)), Ok(false));
    client.script_execute(REMOVE, 1);
    assert_eq!(run(store.remove(&id)), Ok(true));
    client.script_execute(REMOVE_USER, 3);
    assert_eq!(run(store.remove_user("u")), Ok(3));
    client.script_execute(PURGE, 2);
    assert_eq!(run(store.purge_expired(at(9))), Ok(2));
    let executes = client.recorded_executes();
    assert_eq!(executes[2].1, [SqlValue::Text("u".into())]);
    assert_eq!(executes[3].1, [SqlValue::Timestamp(at(9).0)]);
  }

  #[test]
  fn driver_failures_are_storage_errors() {
    let store = PgSessionStore::new(TestSqlClient::new()); // never connected
    let s = session();
    let down = |r: Result<_, SessionError>| matches!(r, Err(SessionError::Storage(_)));
    assert!(down(run(store.save(s.clone()))));
    assert!(down(run(store.load(s.id())).map(|_| ())));
    assert!(down(run(store.remove(s.id())).map(|_| ())));
    assert!(down(run(store.remove_user("u")).map(|_| ())));
    assert!(down(run(store.purge_expired(at(1))).map(|_| ())));
    assert!(down(run(store.ensure_schema())));
  }

  #[test]
  fn ensure_schema_runs_the_migration_statements() {
    let (store, client) = connected();
    run(store.ensure_schema()).unwrap();
    let sql: Vec<String> = client
      .recorded_executes()
      .into_iter()
      .map(|(sql, _)| sql)
      .collect();
    assert_eq!(sql.len(), 3, "{sql:?}");
    assert!(sql[0].starts_with("CREATE TABLE IF NOT EXISTS id_effect_sessions"));
    assert!(sql[1].contains("IF NOT EXISTS id_effect_sessions_user_id_idx"));
    assert!(sql[2].contains("(absolute_expires_at)"));
    assert!(sql.iter().all(|s| !s.contains("--") && !s.ends_with(';')));
  }

  #[test]
  fn statements_drop_comments_and_blank_chunks() {
    assert_eq!(
      statements("-- a; b\nSELECT 1;\n\n  -- c\nSELECT 2;\n;"),
      ["SELECT 1", "SELECT 2"]
    );
  }

  #[test]
  fn the_layer_provides_the_store_over_the_client() {
    let (_, client) = connected();
    let s = session();
    client.script_query(LOAD, vec![row(&s)]);
    let id = s.id().clone();
    let program: Effect<Option<Session>, SessionError, SessionStoreEnv> =
      need::<SessionStoreService>().flat_map(move |st| st.load(&id).narrow());
    let layer = provide_sql_client::<SessionError>(client).provide_to(pg_session_store_layer());
    assert_eq!(run(program.provide_layer(layer)), Ok(Some(s)));
    assert!(format!("{:?}", connected().0).contains("id_effect_sessions"));
  }
}
