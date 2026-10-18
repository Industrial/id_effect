//! **PostgreSQL driver** (feature `pg`) — sqlx [`PgPool`](sqlx::PgPool) and
//! [`PgSqlClient`] implementing [`SqlClient`](crate::SqlClient).
//!
//! [`PgSqlClient::layer`] is the scoped client layer: it opens the pool when it is built and
//! closes it when its scope closes, providing both [`SqlClientService`](crate::SqlClientService)
//! and the shared [`PgPool`] ([`PgSqlEnv`]). Its effects need a tokio runtime with its IO
//! driver (sqlx): run them on `id_effect_tokio::TokioRuntime` or a `TokioExecutor`.
//!
//! ## TLS
//!
//! Connections negotiate TLS through rustls (aws-lc-rs). The mode comes from the URL's `sslmode`
//! (with `sslrootcert`), or from [`PgPoolConfig::with_tls_mode`] /
//! [`PgPoolConfig::with_root_cert_file`], which override it. The default, [`PgTlsMode::Prefer`],
//! encrypts when the server offers TLS but checks no certificate and falls back to plaintext, so
//! it does not stop an active attacker: use [`PgTlsMode::VerifyFull`] in production.
//!
//! See `docs/platform/adrs/adr-sql-driver-choice.md`.

mod client;
mod config;
mod datetime;
mod error;
mod pool_key;
mod providers;
mod stream;
mod transaction;
mod value;

pub use client::PgSqlClient;
pub use config::{
  PgPoolConfig, PgRootCert, PgTlsMode, pg_pool_from_config, pg_pool_from_config_lazy,
};
pub use error::PgSqlError;
pub use pool_key::{PgPool, PgPoolEnv, provide_pg_pool};
pub use providers::{PgSqlEnv, provide_pg_sql_client};
pub use transaction::PgSqlTransaction;
