//! [`Migration`]: the `todos` schema and how it is applied.

use std::sync::Arc;

use id_effect::Effect;
use id_effect_sql::{SqlClient, SqlError, SqlParam, with_transaction};

/// The schema migration of the PostgreSQL store (`migrations/0001_init.sql`), applied by
/// [`Self::apply`] when the [`PgTodoRepositoryLive`](super::PgTodoRepositoryLive) layer is built.
#[derive(Clone, Copy, Debug)]
pub struct Migration;

impl Migration {
  /// The schema script (`migrations/0001_init.sql`).
  pub const SCRIPT_0001: &'static str = include_str!("../../../migrations/0001_init.sql");

  /// Advisory lock key held while [`Self::apply`] runs (one migrator at a time).
  pub const LOCK_KEY: i64 = 0x0007_0dd0_0001;

  /// Applies [`Self::SCRIPT_0001`] on `sql` in one transaction holding the advisory lock
  /// [`Self::LOCK_KEY`], so concurrent starts (and parallel tests) do not race on the DDL.
  /// Idempotent: every statement is `IF NOT EXISTS`.
  pub fn apply(sql: Arc<dyn SqlClient>) -> Effect<(), SqlError> {
    with_transaction(sql, |tx| {
      let lock = tx
        .execute(
          "SELECT pg_advisory_xact_lock($1)",
          &[SqlParam::Int(Self::LOCK_KEY)],
        )
        .map(drop);
      Self::statements(Self::SCRIPT_0001)
        .into_iter()
        .fold(lock, move |done, stmt| {
          let tx = Arc::clone(&tx);
          done.flat_map(move |()| tx.execute(&stmt, &[]).map(drop))
        })
    })
  }

  /// The statements of a migration script: `--` comment lines dropped, split on `;` (the
  /// scripts contain no `;` inside literals or bodies).
  pub fn statements(script: &str) -> Vec<String> {
    let code: Vec<&str> = script
      .lines()
      .filter(|line| !line.trim_start().starts_with("--"))
      .collect();
    code
      .join("\n")
      .split(';')
      .map(str::trim)
      .filter(|s| !s.is_empty())
      .map(str::to_owned)
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use id_effect::Exit;
  use id_effect_sql::TestSqlClient;

  use super::*;
  use crate::infrastructure::postgres::LivePg;

  #[test]
  fn the_script_splits_into_statements() {
    let stmts = Migration::statements(Migration::SCRIPT_0001);
    assert_eq!(stmts.len(), 1, "{stmts:?}");
    assert!(stmts[0].starts_with("CREATE TABLE IF NOT EXISTS todos"));
    assert_eq!(
      Migration::statements("-- only\n;\n SELECT 1; SELECT 2;"),
      ["SELECT 1", "SELECT 2"]
    );
  }

  #[test]
  fn apply_locks_then_runs_every_statement_in_one_transaction() {
    let client = TestSqlClient::new();
    assert!(matches!(client.connect().block_on(), Exit::Success(())));
    let applied = Migration::apply(Arc::new(client.clone())).block_on();
    assert!(matches!(applied, Exit::Success(())), "{applied:?}");
    let executed: Vec<String> = client
      .recorded_executes()
      .into_iter()
      .map(|(sql, _)| sql)
      .collect();
    assert_eq!(executed.len(), 2, "{executed:?}");
    assert!(executed[0].contains("pg_advisory_xact_lock"));
    assert!(executed[1].starts_with("CREATE TABLE IF NOT EXISTS todos"));
    assert_eq!(client.commits(), 1);
  }

  #[test]
  fn apply_fails_without_a_connection() {
    let applied = Migration::apply(Arc::new(TestSqlClient::new())).block_on();
    assert!(matches!(
      applied,
      Exit::Failure(id_effect::Cause::Fail(SqlError::NotConnected))
    ));
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn live_postgres_migration_is_idempotent() {
    // `connect` applies the migration once; a second run changes nothing and succeeds.
    let Some(pg) = LivePg::connect("live_postgres_migration_is_idempotent").await else {
      return;
    };
    let again = LivePg::exit(Migration::apply(Arc::clone(&pg.sql.0))).await;
    assert!(matches!(again, Exit::Success(())), "{again:?}");
    pg.close().await;
  }
}
