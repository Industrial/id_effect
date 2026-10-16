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
pub mod health;
pub mod metrics;
pub mod row;
pub mod transaction;
pub mod value;

#[cfg(feature = "pg")]
pub mod pg;

#[cfg(feature = "proptest")]
pub mod testing;

#[allow(deprecated)]
pub use client::SqlParam;
pub use client::{
  SqlClient, SqlClientEnv, SqlClientService, TestSqlClient, execute, provide_sql_client, query,
  sql_client_env, sql_client_layer,
};
pub use error::SqlError;
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
