//! **SQL client** traits for `id_effect` — driver-agnostic database access inspired by
//! Effect.ts [`@effect/sql`](https://effect.website/docs/sql/introduction).
//!
//! Production PostgreSQL support is behind the `pg` feature ([`pg`] module, sqlx `PgPool`).
//! See [`docs/platform/adrs/adr-sql-driver-choice.md`](../../docs/platform/adrs/adr-sql-driver-choice.md).
//!
//! ## Example
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use id_effect::{Effect, run_blocking};
//! use id_effect_sql::{SqlClient, SqlError, SqlParam, SqlRow, TestSqlClient, transaction_scope};
//!
//! fn count_users(db: Arc<dyn SqlClient>) -> Effect<usize, SqlError, ()> {
//!   db.query("SELECT id FROM users WHERE active = $1", &[SqlParam::Bool(true)])
//!     .map(|rows| rows.len())
//! }
//!
//! let test = TestSqlClient::new();
//! test.script_query(
//!   "SELECT id FROM users WHERE active = $1",
//!   vec![SqlRow::new(vec!["1".into()]), SqlRow::new(vec!["2".into()])],
//! );
//! let db: Arc<dyn SqlClient> = Arc::new(test.clone());
//! run_blocking(db.connect(), ()).unwrap();
//! assert_eq!(run_blocking(count_users(db.clone()), ()), Ok(2));
//!
//! let n = run_blocking(
//!   transaction_scope(db, |_tx| Effect::new(|_r: &mut ()| Ok::<u32, SqlError>(0))),
//!   (),
//! )
//! .unwrap();
//! assert_eq!(n, 0);
//! assert_eq!(test.open_transactions(), 0);
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
  private_interfaces,
  private_bounds,
  clippy::new_ret_no_self,
  clippy::unused_unit
)]

pub mod client;
pub mod error;
pub mod transaction;

#[cfg(feature = "pg")]
pub mod pg;

pub use client::{SqlClient, SqlClientService, SqlParam, SqlRow, TestSqlClient, transaction_scope};
pub use error::SqlError;
pub use transaction::{SqlTransaction, with_transaction};
