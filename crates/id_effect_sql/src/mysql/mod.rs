//! **MySQL driver** (feature `mysql`) — sqlx [`MySqlPool`](sqlx::MySqlPool) and
//! [`MySqlSqlClient`] implementing [`SqlClient`](crate::SqlClient). Tested against MySQL 8.
//!
//! [`MySqlSqlClient::layer`] is the scoped client layer: it opens the pool when it is built and
//! closes it when its scope closes. Effects need a tokio runtime (sqlx): run them on
//! `id_effect_tokio::TokioRuntime` or a `TokioExecutor`. Parameters are `?` placeholders, bound
//! in order (statements are prepared, so values never reach the SQL text).
//!
//! ## Values
//!
//! | `SqlValue` | bound as | read back from |
//! |---|---|---|
//! | `Null` | NULL (MySQL NULL is untyped) | any NULL |
//! | `Bool` | `TINYINT` 0 / 1 | `BOOLEAN` = `TINYINT(1)` holding 0 / 1 (another value is [`SqlError::ValueOutOfRange`](crate::SqlError::ValueOutOfRange)) |
//! | `I16` / `I32` / `I64` | `SMALLINT` / `INT` / `BIGINT` | `TINYINT`, `SMALLINT` → `I16`; `MEDIUMINT`, `INT` → `I32`; `BIGINT` → `I64` |
//! | (unsigned) | — | `TINYINT UNSIGNED`, `YEAR` → `I16`; `SMALLINT` / `MEDIUMINT UNSIGNED` → `I32`; `INT` / `BIGINT UNSIGNED` → `I64` (beyond `i64::MAX`: `ValueOutOfRange`) |
//! | `F32` / `F64` | `FLOAT` / `DOUBLE` | `FLOAT` → `F32`, `DOUBLE` → `F64` |
//! | `Text` | text (utf8mb4) | `CHAR`, `VARCHAR`, `*TEXT`, `ENUM`, `SET` |
//! | `Bytes` | bytes | `BINARY`, `VARBINARY`, `*BLOB` |
//! | `Uuid` | 16 bytes | a `BINARY(16)` column reads back as `Bytes` (`row.get::<Uuid>` takes 16 bytes) |
//! | `Timestamp` | literal `'YYYY-MM-DD hh:mm:ss.ffffff'`, the UTC wall clock | `TIMESTAMP` (connections use the UTC session time zone) |
//! | `DateTime` | literal `'YYYY-MM-DD hh:mm:ss.ffffff'` | `DATETIME` |
//! | `Date` / `Time` | literal `'YYYY-MM-DD'` / `TIME` | `DATE` / `TIME` within a day (a negative or ≥ 24h `TIME` is `ValueOutOfRange`) |
//! | `Decimal` | exact text | `DECIMAL` (trailing zeros beyond 28 digits dropped; more digits: `ValueOutOfRange`) |
//! | `Json` | JSON text | `JSON` |
//!
//! Other column types (`BIT`, `GEOMETRY`) are [`SqlError::Unsupported`](crate::SqlError::Unsupported)
//! naming the column; cast them. sqlx has no public parameter type for `DATE` / `DATETIME` /
//! `TIMESTAMP`, so those bind as literals the server converts when it assigns or compares them;
//! a bare `SELECT ?` of one reads back as `Text` (`CAST(? AS DATETIME(6))` keeps the type).
//! Declare fractional seconds to keep them: `DATETIME(6)`,
//! `TIMESTAMP(6)`, `TIME(6)` (without, MySQL rounds them away); sub-microsecond digits are
//! truncated. Dates bind in years 0000-9999, but a `TIMESTAMP` column only holds 1970-01-01 to
//! 2038-01-19 (UTC): store wider instants in `DATETIME(6)` as UTC wall-clock time. MySQL
//! normalises `JSON` (key order, whitespace); the values read back are equal.
//!
//! ## Transactions
//!
//! A transaction begins with `START TRANSACTION`, or `START TRANSACTION READ ONLY` (writes then
//! fail). An isolation level is set with `SET TRANSACTION ISOLATION LEVEL …` just before, in the
//! same round trip (MySQL sets the *next* transaction's level); without one the server's default
//! applies (`REPEATABLE READ`). `DEFERRABLE` has no MySQL equivalent and changes nothing.
//! Savepoints are MySQL savepoints. A deadlock (`ER_LOCK_DEADLOCK`, 1213) is SQLSTATE `40001`,
//! and a lock wait timeout (`ER_LOCK_WAIT_TIMEOUT`, 1205) is reported as `40001` too, so
//! [`retry_serialization_failures`](crate::retry_serialization_failures) retries both; the
//! message of every server error ends with its SQLSTATE (`"… (SQLSTATE 22007)"`,
//! [`SqlError::sqlstate`](crate::SqlError::sqlstate)), a duplicate key (`23000`) reported as
//! `23505` like PostgreSQL's unique violation.
//!
//! ## Streaming
//!
//! [`query_stream`](crate::SqlClient::query_stream) reads the result as it arrives, buffering at
//! most two batches of 64 rows. Dropping or interrupting the stream sends `KILL QUERY` for the
//! statement from a separate connection; the pooled connection then reads what the server
//! already sent before it is reused.
//!
//! ## Connections
//!
//! `MySqlPoolConfig::url` takes sqlx's MySQL URL (TCP, or a unix socket with `?socket=`).
//! Connections negotiate TLS through rustls (aws-lc-rs). The mode comes from the URL's
//! `ssl-mode` (with `ssl-ca`), or from [`MySqlPoolConfig::with_tls_mode`] /
//! [`MySqlPoolConfig::with_root_cert_file`], which override it. The default,
//! [`MySqlTlsMode::Preferred`], encrypts when the server offers TLS but checks no certificate and
//! falls back to plaintext, so it does not stop an active attacker: use
//! [`MySqlTlsMode::VerifyIdentity`] in production. Over TLS, `caching_sha2_password` (the MySQL 8
//! default) sends the password inside the encrypted session; over plaintext it is RSA-encrypted
//! with a server public key that is not authenticated.

mod client;
mod config;
mod error;
mod stream;
mod transaction;
mod value;

pub use client::MySqlSqlClient;
pub use config::{
  MySqlPoolConfig, MySqlRootCert, MySqlTlsMode, mysql_pool_from_config, mysql_pool_from_config_lazy,
};
pub use error::MySqlSqlError;
pub use transaction::MySqlSqlTransaction;
