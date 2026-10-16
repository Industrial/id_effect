//! [`PgTodoRepositoryLive`]: the PostgreSQL store as a layer.

use std::sync::Arc;

use id_effect::{Layer, caps, hlist, need};
use id_effect_sql::{SqlClientService, SqlError};

use super::{Migration, PgTodoRepository};
use crate::domain::todo::TodoRepositoryService;

/// Provides [`TodoRepositoryService`] over a [`PgTodoRepository`] on the [`SqlClientService`]
/// capability (`PgTodoRepositoryLive::layer()`), the PostgreSQL counterpart of
/// [`InMemoryTodoRepositoryLive`](crate::infrastructure::memory::InMemoryTodoRepositoryLive).
#[derive(Clone, Copy, Debug)]
pub struct PgTodoRepositoryLive;

impl PgTodoRepositoryLive {
  /// The layer (Effect.ts `Layer.effect(TodoRepository, ..)`): building it reads the
  /// [`SqlClientService`] (normally from [`database_layer`](super::database_layer)), applies
  /// [`Migration::apply`] on it, then provides the repository. A failed migration fails the
  /// build with `E::from(err)`.
  pub fn layer<E>() -> Layer<caps!(TodoRepositoryService), E, caps!(SqlClientService)>
  where
    E: From<SqlError> + Send + 'static,
  {
    Layer::effect(|| {
      need::<SqlClientService>().flat_map(|sql: Arc<SqlClientService>| {
        let sql = SqlClientService::clone(&sql);
        Migration::apply(Arc::clone(&sql.0))
          .map_err(E::from)
          .narrow()
          .map(move |()| hlist![TodoRepositoryService::new(PgTodoRepository::new(sql))])
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Effect, Exit};
  use id_effect_sql::{SqlClient as _, SqlValue, TestSqlClient};

  use super::*;
  use crate::domain::todo::StoreError;

  /// The layer over a scripted client.
  fn over(client: &TestSqlClient) -> Layer<caps!(TodoRepositoryService), SqlError> {
    let sql: Layer<caps!(SqlClientService), SqlError> =
      Layer::succeed(hlist![SqlClientService::new(client.clone())]);
    sql.provide_to(PgTodoRepositoryLive::layer::<SqlError>())
  }

  fn count() -> Effect<usize, SqlError, caps!(TodoRepositoryService)> {
    need::<TodoRepositoryService>().flat_map(|repo| {
      repo
        .all()
        .map(|todos| todos.len())
        .map_err(|e: StoreError| SqlError::Unsupported(e.to_string()))
        .narrow()
    })
  }

  #[test]
  fn building_migrates_then_provides_the_repository() {
    let client = TestSqlClient::new();
    assert!(matches!(client.connect().block_on(), Exit::Success(())));
    client.script_query(
      PgTodoRepository::SELECT_ALL,
      vec![id_effect_sql::SqlRow::from_pairs([
        ("id", SqlValue::I64(1)),
        ("title", SqlValue::Text("a".into())),
        ("completed", SqlValue::Bool(false)),
      ])],
    );
    let counted = count().provide_layer(over(&client)).block_on();
    assert!(matches!(counted, Exit::Success(1)), "{counted:?}");
    assert_eq!(client.commits(), 1, "the migration committed");
    assert!(
      client
        .recorded_executes()
        .iter()
        .any(|(sql, _)| sql.starts_with("CREATE TABLE IF NOT EXISTS todos"))
    );
  }

  #[test]
  fn a_failed_migration_fails_the_build() {
    let client = TestSqlClient::new();
    let counted = count().provide_layer(over(&client)).block_on();
    assert!(matches!(
      counted,
      Exit::Failure(Cause::Fail(SqlError::NotConnected))
    ));
  }
}
