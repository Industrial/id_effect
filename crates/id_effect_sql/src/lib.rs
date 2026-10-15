//! **SQL client** traits for `id_effect` — driver-agnostic database access inspired by
//! Effect.ts [`@effect/sql`](https://effect.website/docs/sql/introduction), on the
//! `id_effect::next` kernel.
//!
//! - [`SqlClient`] returns `next::Effect`s; the capability is [`SqlClientService`]
//!   ([`SqlClientEnv`]), read by [`query`] / [`execute`] / [`transaction`](fn@transaction).
//! - The client is a scoped `Layer`: [`sql_client_layer`] connects it when built and closes it
//!   when the scope closes (`pg::PgSqlClient::layer` opens and closes a pool).
//! - A transaction is a bracket ([`with_transaction`], [`transaction`](fn@transaction)) or a
//!   scoped `Layer` ([`transaction_layer`]): it commits on success and rolls back on failure,
//!   defect or interrupt.
//!
//! Production PostgreSQL support is behind the `pg` feature (`pg` module, sqlx `PgPool`).
//! See [`docs/platform/adrs/adr-sql-driver-choice.md`](../../docs/platform/adrs/adr-sql-driver-choice.md).
//!
//! ## Example
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect::next::Effect;
//! use id_effect_sql::{
//!   SqlClientEnv, SqlError, SqlParam, SqlRow, TestSqlClient, query, sql_client_layer, transaction,
//! };
//!
//! fn count_users() -> Effect<usize, SqlError, SqlClientEnv> {
//!   query("SELECT id FROM users WHERE active = $1", vec![SqlParam::Bool(true)])
//!     .map(|rows| rows.len())
//! }
//!
//! let test = TestSqlClient::new();
//! test.script_query(
//!   "SELECT id FROM users WHERE active = $1",
//!   vec![SqlRow::new(vec!["1".into()]), SqlRow::new(vec!["2".into()])],
//! );
//!
//! // Connected while the program runs, closed after it.
//! let program = count_users()
//!   .flat_map(|n| {
//!     let n = SqlParam::Int(n as i64);
//!     transaction(move |tx| tx.execute("UPDATE stats SET n = $1", &[n]).narrow())
//!   })
//!   .provide_layer(sql_client_layer::<SqlError>(test.clone()));
//! assert!(matches!(program.block_on(), Exit::Success(0)));
//! assert_eq!((test.commits(), test.open_transactions(), test.closes()), (1, 0, 1));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod client;
pub mod error;
pub mod transaction;

#[cfg(feature = "pg")]
pub mod pg;

pub use client::{
  SqlClient, SqlClientEnv, SqlClientService, SqlParam, SqlRow, TestSqlClient, execute,
  provide_sql_client, query, sql_client_env, sql_client_layer,
};
pub use error::SqlError;
pub use transaction::{
  SqlTransaction, SqlTransactionEnv, SqlTransactionService, transaction, transaction_layer,
  with_transaction,
};

/// Alias of [`with_transaction`] (the 0.6 name).
pub use transaction::with_transaction as transaction_scope;

/// Compiles and runs the README's quick start as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
