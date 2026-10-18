//! Typed statements (feature `query`): build a `SELECT` / `INSERT` / `UPDATE` / `DELETE` with
//! [`sea_query`], render it for a [`Dialect`] with [`BuildSql::build_sql`] — SQL text plus its
//! parameters as [`SqlValue`]s (the [bridge](sql_values) is ours, not `sea-query-binder`, so the
//! [`SqlClient`](crate::SqlClient) abstraction and sqlx's version stay independent) — and run it
//! with [`execute_query`] / [`fetch_all_as`] / [`fetch_one_as`] / [`fetch_optional_as`], the
//! statement counterparts of [`execute`] and [`query_as`] / [`query_one_as`] /
//! [`query_opt_as`].
//!
//! Postgres renders `$1, $2, …`, MySQL and SQLite render `?`: the placeholders each driver binds,
//! so the rendered SQL goes straight to [`SqlClient::query`](crate::SqlClient::query).
//! [`sea_query`] is re-exported so builders and this crate agree on its version. Values go into a
//! builder as themselves (`Expr::val(7)`, jiff dates and times included) or, for any
//! [`ToSqlValue`](crate::ToSqlValue) type, as `Expr::val(x.to_sql_value())`.
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait, Order, Query};
//! use id_effect_sql::query_builder::{BuildSql, Dialect, fetch_all_as};
//! use id_effect_sql::{
//!   FromRow, SqlClientEnv, SqlError, SqlRow, SqlValue, TestSqlClient, sql_client_layer,
//! };
//!
//! #[derive(Debug, PartialEq, FromRow)]
//! struct Todo {
//!   id: i64,
//!   title: String,
//! }
//!
//! let open = Query::select()
//!   .columns(["id", "title"])
//!   .from("todos")
//!   .and_where(Expr::col("done").eq(false))
//!   .order_by("id", Order::Asc)
//!   .limit(10)
//!   .to_owned();
//!
//! let (sql, params) = open.build_sql(Dialect::Postgres)?;
//! assert_eq!(
//!   sql,
//!   r#"SELECT "id", "title" FROM "todos" WHERE "done" = $1 ORDER BY "id" ASC LIMIT $2"#
//! );
//! assert_eq!(params, [SqlValue::Bool(false), SqlValue::I64(10)]);
//! let (mysql, _) = open.build_sql(Dialect::MySql)?;
//! assert!(mysql.starts_with("SELECT `id`, `title` FROM `todos` WHERE `done` = ?"));
//!
//! let test = TestSqlClient::new();
//! test.script_query(sql, vec![SqlRow::from_pairs([("id", SqlValue::I64(1)), ("title", "docs".into())])]);
//! let todos = fetch_all_as::<Todo, SqlClientEnv, _>(Dialect::Postgres, &open)
//!   .provide_layer(sql_client_layer::<SqlError>(test));
//! assert!(matches!(todos.block_on(), Exit::Success(v) if v == [Todo { id: 1, title: "docs".into() }]));
//! # Ok::<(), SqlError>(())
//! ```

use id_effect::{Effect, HList, Selector, fail};
pub use sea_query;
use sea_query::{
  MysqlQueryBuilder, PostgresQueryBuilder, QueryStatementWriter, SqliteQueryBuilder,
};

use crate::client::{SqlClientService, execute};
use crate::error::SqlError;
use crate::from_row::{FromRow, query_as, query_one_as, query_opt_as};
use crate::value::SqlValue;

mod value;

pub use value::sql_values;

/// The SQL dialect a statement is rendered in (quoting, placeholders, `RETURNING`, upserts).
///
/// Non-exhaustive: more dialects may be added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Dialect {
  /// PostgreSQL: `"ident"`, placeholders `$1, $2, …`.
  Postgres,
  /// MySQL: `` `ident` ``, placeholders `?`.
  MySql,
  /// SQLite: `"ident"`, placeholders `?`.
  Sqlite,
}

/// A statement that renders to SQL text plus [`SqlValue`] parameters: every sea-query query
/// statement (`SelectStatement`, `InsertStatement`, `UpdateStatement`, `DeleteStatement`,
/// `WithQuery`).
pub trait BuildSql {
  /// The statement's SQL in `dialect` and its parameters in placeholder order.
  /// [`SqlError::Unsupported`] or [`SqlError::ValueOutOfRange`] (naming the parameter, `$2`)
  /// when a parameter has no [`SqlValue`] (see [`sql_values`]).
  fn build_sql(&self, dialect: Dialect) -> Result<(String, Vec<SqlValue>), SqlError>;
}

impl<S: QueryStatementWriter> BuildSql for S {
  fn build_sql(&self, dialect: Dialect) -> Result<(String, Vec<SqlValue>), SqlError> {
    let (sql, values) = match dialect {
      Dialect::Postgres => self.build(PostgresQueryBuilder),
      Dialect::MySql => self.build(MysqlQueryBuilder),
      Dialect::Sqlite => self.build(SqliteQueryBuilder),
    };
    Ok((sql, sql_values(values)?))
  }
}

/// Render `stmt` now and run `run` on it, or fail with the rendering error.
fn with_built<A, R>(
  dialect: Dialect,
  stmt: &(impl BuildSql + ?Sized),
  run: impl FnOnce(String, Vec<SqlValue>) -> Effect<A, SqlError, R>,
) -> Effect<A, SqlError, R>
where
  A: Send + 'static,
  R: 'static,
{
  match stmt.build_sql(dialect) {
    Ok((sql, params)) => run(sql, params),
    Err(e) => fail(e),
  }
}

/// [`execute`] for a statement rendered in `dialect` (`INSERT` / `UPDATE` / `DELETE`): the
/// affected row count. The statement is rendered when this is called; a rendering error fails the
/// effect.
pub fn execute_query<R, I>(
  dialect: Dialect,
  stmt: &(impl BuildSql + ?Sized),
) -> Effect<u64, SqlError, R>
where
  R: Selector<SqlClientService, I> + HList,
{
  with_built(dialect, stmt, execute::<R, I>)
}

/// [`query_as`] for a statement rendered in `dialect`: every row decoded as `T`
/// (`fetch_all_as::<Todo, _, _>(Dialect::Postgres, &select)` where the return type fixes the
/// environment, else `fetch_all_as::<Todo, SqlClientEnv, _>(..)`; [`SqlRow`](crate::SqlRow) for
/// raw rows). Also for `INSERT … RETURNING`.
pub fn fetch_all_as<T, R, I>(
  dialect: Dialect,
  stmt: &(impl BuildSql + ?Sized),
) -> Effect<Vec<T>, SqlError, R>
where
  T: FromRow + Send + 'static,
  R: Selector<SqlClientService, I> + HList,
{
  with_built(dialect, stmt, query_as::<T, R, I>)
}

/// [`query_one_as`] for a statement rendered in `dialect`: the first row as `T`,
/// [`SqlError::RowNotFound`] when there is none.
pub fn fetch_one_as<T, R, I>(
  dialect: Dialect,
  stmt: &(impl BuildSql + ?Sized),
) -> Effect<T, SqlError, R>
where
  T: FromRow + Send + 'static,
  R: Selector<SqlClientService, I> + HList,
{
  with_built(dialect, stmt, query_one_as::<T, R, I>)
}

/// [`query_opt_as`] for a statement rendered in `dialect`: the first row as `T`, or `None`.
pub fn fetch_optional_as<T, R, I>(
  dialect: Dialect,
  stmt: &(impl BuildSql + ?Sized),
) -> Effect<Option<T>, SqlError, R>
where
  T: FromRow + Send + 'static,
  R: Selector<SqlClientService, I> + HList,
{
  with_built(dialect, stmt, query_opt_as::<T, R, I>)
}

#[cfg(test)]
mod tests;
