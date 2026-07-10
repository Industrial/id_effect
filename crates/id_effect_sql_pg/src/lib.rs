//! **PostgreSQL driver** for [`id_effect_sql`] — sqlx [`PgPool`](sqlx::PgPool) and
//! [`PgSqlClient`] implementing [`SqlClient`](id_effect_sql::SqlClient).
//!
//! See ADR [`adr-sql-driver-choice.md`](../../docs/platform/adrs/adr-sql-driver-choice.md).

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(clippy::new_ret_no_self, clippy::unused_unit)]

mod client;
mod config;
mod error;
mod pool_key;
mod providers;
mod transaction;

pub use client::PgSqlClient;
pub use config::{PgPoolConfig, pg_pool_from_config, pg_pool_from_config_lazy};
pub use error::PgSqlError;
pub use pool_key::{PgPool, provide_pg_pool};
pub use providers::provide_pg_sql_client;
pub use transaction::PgSqlTransaction;
