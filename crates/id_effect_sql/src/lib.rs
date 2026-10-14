//! **SQL client** traits for `id_effect` — driver-agnostic database access inspired by
//! Effect.ts [`@effect/sql`](https://effect.website/docs/sql/introduction).
//!
//! Production PostgreSQL support is behind the `pg` feature ([`pg`] module, sqlx `PgPool`).
//! See [`docs/platform/adrs/adr-sql-driver-choice.md`](../../docs/platform/adrs/adr-sql-driver-choice.md).

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
