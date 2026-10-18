//! [`PgAccountTokenStore`]: an [`AccountTokenStore`] in PostgreSQL over `id_effect_sql`'s
//! [`SqlClient`].

use std::fmt;

use id_effect::{Effect, UtcDateTime, fail, succeed};
use id_effect_sql::{SqlClient, SqlClientService, SqlError, SqlRow, params};

use super::{AccountError, AccountTokenStore, TokenKind, TokenRecord};
use crate::session::statements;

/// The crate's migration `migrations/0002_id_effect_account_tokens.up.sql`: what
/// [`PgAccountTokenStore::ensure_schema`] runs.
const SCHEMA: &str = include_str!("../../migrations/0002_id_effect_account_tokens.up.sql");

const INSERT: &str = "INSERT INTO id_effect_account_tokens \
  (id, kind, account_id, secret_hash, expires_at) VALUES ($1, $2, $3, $4, $5)";

const FIND: &str = "SELECT kind, account_id, secret_hash, expires_at \
  FROM id_effect_account_tokens WHERE id = $1";

const CONSUME: &str = "DELETE FROM id_effect_account_tokens WHERE id = $1";

const REMOVE_ACCOUNT: &str =
  "DELETE FROM id_effect_account_tokens WHERE account_id = $1 AND kind = $2";

const PURGE: &str = "DELETE FROM id_effect_account_tokens WHERE expires_at <= $1";

/// [`AccountTokenStore`] in the PostgreSQL table `id_effect_account_tokens`, over any
/// [`SqlClient`] (`id_effect_sql::pg::PgSqlClient` in production). Clones share the client.
///
/// - The primary key is the token's public id; the secret is stored only as the lowercase hex
///   SHA-256 `secret_hash`, so a leaked table or backup yields no usable link.
/// - [`consume`](AccountTokenStore::consume) is one `DELETE … WHERE id = $1`: of two requests
///   presenting the same token, the database lets exactly one delete the row.
/// - [`purge_expired`](AccountTokenStore::purge_expired) deletes the rows whose `expires_at` is
///   at or before `now`.
///
/// Create the table with the crate's `migrations/0002_id_effect_account_tokens.up.sql` or with
/// [`ensure_schema`](Self::ensure_schema). Driver and decode failures surface as
/// [`AccountError::Storage`].
#[derive(Clone)]
pub struct PgAccountTokenStore {
  client: SqlClientService,
}

impl fmt::Debug for PgAccountTokenStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("PgAccountTokenStore")
      .field("table", &"id_effect_account_tokens")
      .finish_non_exhaustive()
  }
}

impl PgAccountTokenStore {
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

  /// Create `id_effect_account_tokens` and its indexes unless they exist: the statements of
  /// `migrations/0002_id_effect_account_tokens.up.sql`, one at a time.
  pub fn ensure_schema(&self) -> Effect<(), AccountError> {
    let client = self.client.clone();
    statements(SCHEMA)
      .into_iter()
      .fold(succeed(()), move |done, sql| {
        let client = client.clone();
        done.flat_map(move |()| client.execute(&sql, &[]).map(|_| ()).map_err(storage))
      })
  }
}

fn storage(e: SqlError) -> AccountError {
  AccountError::Storage(e.to_string())
}

/// The record `id` from its `FIND` row.
fn decode(id: &str, row: &SqlRow) -> Result<TokenRecord, AccountError> {
  let kind = row.get::<String>("kind").map_err(storage)?;
  let kind = TokenKind::parse(&kind)
    .ok_or_else(|| AccountError::Storage(format!("unknown account token kind `{kind}`")))?;
  TokenRecord::from_parts(
    id,
    kind,
    row.get::<String>("account_id").map_err(storage)?,
    &row.get::<String>("secret_hash").map_err(storage)?,
    UtcDateTime(row.get::<jiff::Timestamp>("expires_at").map_err(storage)?),
  )
}

impl AccountTokenStore for PgAccountTokenStore {
  fn insert(&self, record: TokenRecord) -> Effect<(), AccountError> {
    let params = params![
      record.id(),
      record.kind().as_str(),
      record.account_id(),
      record.digest_hex(),
      record.expires_at().0,
    ];
    self
      .client
      .execute(INSERT, &params)
      .map(|_| ())
      .map_err(storage)
  }

  fn find(&self, id: &str) -> Effect<Option<TokenRecord>, AccountError> {
    let id = id.to_owned();
    self
      .client
      .query(FIND, &params![id.as_str()])
      .map_err(storage)
      .flat_map(move |rows| match rows.first().map(|row| decode(&id, row)) {
        None => succeed(None),
        Some(Ok(record)) => succeed(Some(record)),
        Some(Err(e)) => fail(e),
      })
  }

  fn consume(&self, id: &str) -> Effect<bool, AccountError> {
    self
      .client
      .execute(CONSUME, &params![id])
      .map(|n| n > 0)
      .map_err(storage)
  }

  fn remove_account(&self, account_id: &str, kind: TokenKind) -> Effect<u64, AccountError> {
    self
      .client
      .execute(REMOVE_ACCOUNT, &params![account_id, kind.as_str()])
      .map_err(storage)
  }

  fn purge_expired(&self, now: UtcDateTime) -> Effect<u64, AccountError> {
    self.client.execute(PURGE, &params![now.0]).map_err(storage)
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};
  use id_effect_sql::{SqlValue, TestSqlClient};

  use super::*;
  use crate::account::AccountToken;

  fn run<A: Send + 'static>(eff: Effect<A, AccountError>) -> Result<A, AccountError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn at(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn connected() -> (PgAccountTokenStore, TestSqlClient) {
    let client = TestSqlClient::new();
    run(client.connect().map_err(storage)).unwrap();
    (PgAccountTokenStore::new(client.clone()), client)
  }

  fn row(r: &TokenRecord) -> SqlRow {
    SqlRow::from_pairs([
      ("kind", SqlValue::Text(r.kind().as_str().into())),
      ("account_id", SqlValue::Text(r.account_id().into())),
      ("secret_hash", SqlValue::Text(r.digest_hex())),
      ("expires_at", SqlValue::Timestamp(r.expires_at().0)),
    ])
  }

  #[test]
  fn insert_binds_the_digest_never_the_token() {
    let (store, client) = connected();
    let (token, record) = AccountToken::generate(TokenKind::ResetPassword, "a1", at(9)).unwrap();
    run(store.insert(record.clone())).unwrap();
    let (sql, params) = &client.recorded_executes()[0];
    assert_eq!(sql, INSERT);
    assert_eq!(params[0], SqlValue::Text(record.id().into()));
    assert_eq!(params[1], SqlValue::Text("reset_password".into()));
    assert_eq!(params[3], SqlValue::Text(record.digest_hex()));
    let secret = token.expose().split_once('_').unwrap().1;
    assert!(
      params
        .iter()
        .all(|p| !matches!(p, SqlValue::Text(t) if t.contains(secret))),
      "the secret is never bound"
    );
  }

  #[test]
  fn find_decodes_the_row_and_rejects_bad_ones() {
    let (store, client) = connected();
    let (_, record) = AccountToken::generate(TokenKind::VerifyEmail, "a1", at(9)).unwrap();
    client.script_query(FIND, vec![row(&record)]);
    assert_eq!(run(store.find(record.id())), Ok(Some(record.clone())));
    client.script_query(FIND, vec![]);
    assert_eq!(run(store.find(record.id())), Ok(None));
    let bad = SqlRow::from_pairs([
      ("kind", SqlValue::Text("other".into())),
      ("account_id", SqlValue::Text("a1".into())),
    ]);
    client.script_query(FIND, vec![bad]);
    assert!(
      matches!(run(store.find(record.id())), Err(AccountError::Storage(m)) if m.contains("other"))
    );
  }

  #[test]
  fn deletes_report_the_affected_rows() {
    let (store, client) = connected();
    assert_eq!(run(store.consume("x")), Ok(false));
    client.script_execute(CONSUME, 1);
    assert_eq!(run(store.consume("x")), Ok(true));
    client.script_execute(REMOVE_ACCOUNT, 2);
    assert_eq!(
      run(store.remove_account("a1", TokenKind::VerifyEmail)),
      Ok(2)
    );
    client.script_execute(PURGE, 3);
    assert_eq!(run(store.purge_expired(at(5))), Ok(3));
    let executes = client.recorded_executes();
    assert_eq!(
      executes[2].1,
      [
        SqlValue::Text("a1".into()),
        SqlValue::Text("verify_email".into())
      ]
    );
    assert_eq!(executes[3].1, [SqlValue::Timestamp(at(5).0)]);
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
    assert!(sql[0].starts_with("CREATE TABLE IF NOT EXISTS id_effect_account_tokens"));
    assert!(sql[1].contains("(account_id, kind)"));
    assert!(sql[2].contains("(expires_at)"));
    let down = PgAccountTokenStore::new(TestSqlClient::new()); // never connected
    assert!(matches!(
      run(down.ensure_schema()),
      Err(AccountError::Storage(_))
    ));
    assert!(format!("{down:?}").contains("id_effect_account_tokens"));
  }
}
