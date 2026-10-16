//! **SQLite driver** (feature `sqlite`) — sqlx [`SqlitePool`](sqlx::SqlitePool) and
//! [`SqliteSqlClient`] implementing [`SqlClient`](crate::SqlClient).
//!
//! [`SqliteSqlClient::layer`] is the scoped client layer: it opens the pool when it is built and
//! closes it when its scope closes. [`SqlitePoolConfig::memory`] is a private in-memory
//! database (it lives as long as the pool), [`SqlitePoolConfig::file`] a database file (created
//! if missing, in WAL mode so readers do not wait for a writer). Effects need a tokio runtime
//! (sqlx): run them on `id_effect_tokio::TokioRuntime` or a `TokioExecutor`.
//!
//! ## Values
//!
//! SQLite stores INTEGER, REAL, TEXT and BLOB values whatever a column declares, so a value
//! decodes by its storage class, refined by the column's declared type:
//!
//! | `SqlValue` | bound as | read back from |
//! |---|---|---|
//! | `Null` | NULL | any NULL |
//! | `Bool` | INTEGER 0 / 1 | INTEGER 0 / 1 in a `BOOLEAN` column (another integer is [`SqlError::ValueOutOfRange`](crate::SqlError::ValueOutOfRange)) |
//! | `I16` / `I32` / `I64` | INTEGER | INTEGER → `I64` (`row.get::<i16>` range-checks) |
//! | `F32` / `F64` | REAL | REAL → `F64` |
//! | `Text` / `Bytes` | TEXT / BLOB | TEXT → `Text`, BLOB → `Bytes` |
//! | `Uuid` | TEXT, hyphenated | TEXT (`row.get::<Uuid>` parses it) |
//! | `Timestamp` | TEXT `YYYY-MM-DDTHH:MM:SS.ffffffZ` (UTC) | TEXT with an offset in a `TIMESTAMP` / `DATETIME` column |
//! | `DateTime` | TEXT `YYYY-MM-DD HH:MM:SS.ffffff` | TEXT without an offset in a `TIMESTAMP` / `DATETIME` column |
//! | `Date` / `Time` | TEXT `YYYY-MM-DD` / `HH:MM:SS.ffffff` | TEXT in a `DATE` / `TIME` column |
//! | `Decimal` | TEXT, exact | TEXT (`row.get::<Decimal>` parses it) |
//! | `Json` | TEXT, compact | TEXT (`row.get::<serde_json::Value>` / `Json<T>` parse it) |
//!
//! Numbers and booleans are never parsed from text. Date-time text is fixed-width, so it sorts
//! and compares in time order; sub-microsecond digits are truncated and only years 0–9999 bind.
//! Declare uuid, decimal and JSON columns `TEXT`: a `DECIMAL` or `JSON` column has NUMERIC
//! affinity, and SQLite turns numeric-looking text into a (lossy) number. A column's
//! [`SqlType`](crate::SqlType) is that of its first non-NULL value, else its declared type's.
//!
//! ## Transactions
//!
//! SQLite transactions are serializable whatever the level asked for (stricter than requested,
//! as the SQL standard allows). [`Isolation::Serializable`](crate::Isolation::Serializable)
//! begins with `BEGIN IMMEDIATE`, taking the write lock up front so the transaction cannot fail
//! later on a lock upgrade; the other levels begin `DEFERRED`. `READ ONLY` is
//! [`SqlError::Unsupported`](crate::SqlError::Unsupported) (open the database with `?mode=ro`
//! instead); `DEFERRABLE` changes nothing. Savepoints are SQLite savepoints. `SQLITE_BUSY`
//! ("database is locked") is reported as SQLSTATE `40001`, so
//! [`retry_serialization_failures`](crate::retry_serialization_failures) retries it.
//!
//! An in-memory database shares one cache between its connections: a statement outside a
//! transaction that touches a table the transaction wrote waits until it commits or rolls back.
//! Use a file database for readers that must not wait.

mod client;
mod config;
mod error;
mod stream;
mod transaction;
mod value;

pub use client::SqliteSqlClient;
pub use config::{SqlitePoolConfig, sqlite_pool_from_config, sqlite_pool_from_config_lazy};
pub use error::SqliteSqlError;
pub use transaction::SqliteSqlTransaction;
