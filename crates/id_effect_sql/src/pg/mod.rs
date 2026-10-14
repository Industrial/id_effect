//! **PostgreSQL driver** (feature `pg`) — sqlx [`PgPool`](sqlx::PgPool) and
//! [`PgSqlClient`] implementing [`SqlClient`](crate::SqlClient).
//!
//! Formerly the `id_effect_sql_pg` crate (merged in 0.6, ADR 0010). See
//! `docs/platform/adrs/adr-sql-driver-choice.md`.

#![allow(clippy::unused_unit)]

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
