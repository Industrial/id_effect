//! **SQL client** traits for `id_effect` — driver-agnostic database access inspired by
//! Effect.ts [`@effect/sql`](https://effect.website/docs/sql/introduction), on the
//! 0.7 kernel.
//!
//! - [`SqlClient`] returns `Effect`s; the capability is [`SqlClientService`]
//!   ([`SqlClientEnv`]), read by [`query`] / [`execute`] / [`transaction`](fn@transaction).
//! - The client is a scoped `Layer`: [`sql_client_layer`] connects it when built and closes it
//!   when the scope closes (`pg::PgSqlClient::layer` opens and closes a pool).
//! - A transaction is a bracket ([`with_transaction`], [`transaction`](fn@transaction)) or a
//!   scoped `Layer` ([`transaction_layer`]): it commits on success and rolls back on failure,
//!   defect or interrupt.
//!
//! - Parameters and column values are typed [`SqlValue`]s ([`params!`] encodes Rust values with
//!   [`ToSqlValue`]); a [`SqlRow`] decodes its columns by name or index with
//!   [`SqlRow::get`] ([`FromSqlValue`]), returning a [`SqlError`] naming the column.
//!
//! Production PostgreSQL support is behind the `pg` feature (`pg` module, sqlx `PgPool`).
//! See [`docs/platform/adrs/adr-sql-driver-choice.md`](../../docs/platform/adrs/adr-sql-driver-choice.md).
//!
//! ## Example
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect::{Effect, fail, succeed};
//! use id_effect_sql::{
//!   SqlClientEnv, SqlError, SqlRow, TestSqlClient, params, query, sql_client_layer, transaction,
//! };
//!
//! fn active_ids() -> Effect<Vec<i64>, SqlError, SqlClientEnv> {
//!   query("SELECT id FROM users WHERE active = $1", params![true])
//!     .flat_map(|rows| match rows.iter().map(|r| r.get::<i64>("id")).collect::<Result<_, _>>() {
//!       Ok(ids) => succeed(ids),
//!       Err(e) => fail(e),
//!     })
//! }
//!
//! let test = TestSqlClient::new();
//! test.script_query(
//!   "SELECT id FROM users WHERE active = $1",
//!   vec![SqlRow::from_pairs([("id", 1i64)]), SqlRow::from_pairs([("id", 2i64)])],
//! );
//!
//! // Connected while the program runs, closed after it.
//! let program = active_ids()
//!   .flat_map(|ids| {
//!     let n = ids.len() as i64;
//!     transaction(move |tx| tx.execute("UPDATE stats SET n = $1", &params![n]).narrow())
//!   })
//!   .provide_layer(sql_client_layer::<SqlError>(test.clone()));
//! assert!(matches!(program.block_on(), Exit::Success(0)));
//! assert_eq!((test.commits(), test.open_transactions(), test.closes()), (1, 0, 1));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod client;
pub mod error;
pub mod from_row;
pub mod health;
pub mod metrics;
pub mod row;
pub mod transaction;
pub mod value;

#[cfg(feature = "pg")]
pub mod pg;

#[cfg(feature = "migrate")]
pub mod migrate;

#[cfg(feature = "proptest")]
pub mod testing;

#[allow(deprecated)]
pub use client::SqlParam;
pub use client::{
  SqlClient, SqlClientEnv, SqlClientService, TestSqlClient, execute, provide_sql_client, query,
  sql_client_env, sql_client_layer,
};
pub use error::{SQLSTATE_UNIQUE_VIOLATION, SqlError};
pub use from_row::{FromRow, query_as, query_one_as, query_opt_as};
/// Derive [`FromRow`] for a struct with named fields (see [`from_row`]).
pub use id_effect_proc_macro::FromRow;
pub use row::{ColumnIndex, SqlColumn, SqlRow};
pub use value::{FromSqlValue, Json, SqlType, SqlValue, ToSqlValue, ValueError};

pub use transaction::{
  SqlTransaction, SqlTransactionEnv, SqlTransactionService, transaction, transaction_layer,
  with_transaction,
};
/// The value crates of the public API, re-exported so callers use the same versions.
pub use {jiff, rust_decimal, serde_json, uuid};

/// Alias of [`with_transaction`] (the 0.6 name).
pub use transaction::with_transaction as transaction_scope;

/// Compiles and runs the README's quick start as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

// Savepoints, isolation levels and streaming (E3.1a).
pub mod stream;
pub use stream::{query_stream, query_stream_as};
pub use transaction::{
  Isolation, SQLSTATE_DEADLOCK_DETECTED, SQLSTATE_SERIALIZATION_FAILURE, TransactionOptions,
  is_serialization_failure, retry_serialization_failures, savepoint, transaction_with,
  with_transaction_options,
};

// sea-query statements bridged to `SqlValue` (E3.3, feature `query`). The module is
// `query_builder`: `query` is already the free function.
#[cfg(feature = "query")]
#[path = "query/mod.rs"]
pub mod query_builder;
#[cfg(feature = "query")]
pub use query_builder::{
  BuildSql, Dialect, execute_query, fetch_all_as, fetch_one_as, fetch_optional_as,
};

// SQLite driver (E3.1b, feature `sqlite`) and read-replica routing over any `SqlClient`.
#[cfg(feature = "sqlite")]
pub mod sqlite;

pub mod routing;
pub use routing::{ReadRouting, RoutingSqlClient};

// Hash-chained audit log appended in the caller's transaction (E5.5, feature `audit`).
#[cfg(feature = "audit")]
pub mod audit;

// Entities and repositories: `#[derive(Entity)]`, `Repository<T>` CRUD with optimistic locking
// and global filters (E3.3, feature `repository` = `query` + the repository layer).
#[cfg(feature = "repository")]
pub mod repository;
/// Derive [`Entity`] for a `#[derive(FromRow)]` struct (see [`repository`]).
#[cfg(feature = "repository")]
pub use id_effect_proc_macro::Entity;
#[cfg(feature = "repository")]
pub use repository::{BoundRepository, CurrentTenant, Entity, Page, Repository, SqlExecutor};

// MySQL driver (E3.1b, feature `mysql`).
#[cfg(feature = "mysql")]
pub mod mysql;
