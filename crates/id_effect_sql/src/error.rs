//! Structured errors for SQL client operations.

use std::fmt;

use id_effect::schema::ParseError;

use crate::value::{SqlType, ValueError};

/// SQLSTATE of a unique (or primary key) constraint violation: what
/// [`SqlError::is_unique_violation`] checks for, on every backend.
pub const SQLSTATE_UNIQUE_VIOLATION: &str = "23505";

/// SQL client / query failures.
///
/// Non-exhaustive: match it with a wildcard arm. A `column` field names the column, or `#3` for
/// an index into an unnamed column (`$2` for a bound parameter).
///
/// A server error is [`QueryFailed`](Self::QueryFailed) (or
/// [`TransactionFailed`](Self::TransactionFailed)) whose message ends with its SQLSTATE
/// (`"… (SQLSTATE 23505)"`), on PostgreSQL, MySQL and SQLite alike; read it with
/// [`sqlstate`](Self::sqlstate) and [`is_unique_violation`](Self::is_unique_violation) rather
/// than the message text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SqlError {
  /// Client is not connected (or pool unavailable).
  NotConnected,
  /// The database could not be reached: the connection was refused, reset or timed out at the
  /// network level (sqlx `Error::Io`). A server that answers and rejects the login is
  /// [`QueryFailed`](Self::QueryFailed) with its SQLSTATE.
  #[non_exhaustive]
  ConnectionFailed {
    /// The driver's description of the failure.
    message: String,
  },
  /// TLS could not be established: the server does not offer TLS under a mode that requires it,
  /// its certificate does not verify, or the handshake failed.
  #[non_exhaustive]
  TlsFailed {
    /// The driver's description of the failure.
    message: String,
  },
  /// Parameterized query failed to execute.
  QueryFailed {
    /// SQL text (may be redacted in logs at higher layers).
    sql: String,
    /// Human-readable driver or test-double message.
    message: String,
  },
  /// Transaction could not begin, commit, or roll back.
  TransactionFailed(String),
  /// Row or column could not be decoded.
  Decode(String),
  /// Operation is unsupported by the current implementation.
  Unsupported(String),
  /// A row has no column of that name or index.
  ColumnNotFound {
    /// The column asked for.
    column: String,
  },
  /// A column is NULL but the requested type is not an `Option`.
  UnexpectedNull {
    /// The column.
    column: String,
  },
  /// A column's value is not of a type the requested Rust type decodes from.
  TypeMismatch {
    /// The column.
    column: String,
    /// The type the requested Rust type decodes from.
    expected: SqlType,
    /// The column value's type.
    found: SqlType,
  },
  /// A column's value does not fit the requested Rust type (or the database value does not fit
  /// a [`SqlValue`](crate::SqlValue), e.g. pg `infinity` or `NaN`).
  ValueOutOfRange {
    /// The column.
    column: String,
    /// What did not fit.
    message: String,
  },
  /// A row failed its type's `Schema` ([`from_row_with_schema`](crate::from_row::from_row_with_schema),
  /// `#[from_row(schema)]`). Each issue's `path` starts with the column name.
  InvalidRow {
    /// Every issue the schema reported.
    issues: Vec<ParseError>,
  },
  /// A query that must return a row returned none ([`query_one_as`](crate::query_one_as)).
  RowNotFound,
  /// An optimistic-locking update found the row but not the version the entity was read with:
  /// another writer changed it first (`Repository::update`, feature `repository`). Reload and
  /// retry.
  StaleVersion {
    /// The entity's Rust type name.
    entity: String,
    /// The row's primary key (`Debug`-formatted).
    id: String,
  },
}

impl SqlError {
  /// A [`ConnectionFailed`](Self::ConnectionFailed) error.
  pub fn connection_failed(message: impl Into<String>) -> Self {
    SqlError::ConnectionFailed {
      message: message.into(),
    }
  }

  /// A [`TlsFailed`](Self::TlsFailed) error.
  pub fn tls_failed(message: impl Into<String>) -> Self {
    SqlError::TlsFailed {
      message: message.into(),
    }
  }

  /// The SQLSTATE the database reported for this error (`"23505"`, `"40001"`, …), if it is a
  /// server error that carries one. SQLite reports a unique violation as `23505` and a busy
  /// database as `40001`; MySQL reports a duplicate key as `23505` and a lock wait timeout as
  /// `40001`.
  pub fn sqlstate(&self) -> Option<&str> {
    let message = match self {
      SqlError::QueryFailed { message, .. } | SqlError::TransactionFailed(message) => message,
      _ => return None,
    };
    let code = message.rsplit_once("(SQLSTATE ")?.1.strip_suffix(')')?;
    (code.len() == 5 && code.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(code)
  }

  /// Whether this is a unique (or primary key) constraint violation
  /// ([`SQLSTATE_UNIQUE_VIOLATION`]): an insert or update hit a key another row already has.
  pub fn is_unique_violation(&self) -> bool {
    self.sqlstate() == Some(SQLSTATE_UNIQUE_VIOLATION)
  }

  /// The [`SqlError`] for a [`ValueError`] decoding `column`
  /// ([`ValueError::Invalid`] becomes [`SqlError::Decode`] naming the column).
  pub fn from_value_error(column: impl Into<String>, err: ValueError) -> Self {
    let column = column.into();
    match err {
      ValueError::UnexpectedNull => SqlError::UnexpectedNull { column },
      ValueError::Mismatch { expected, found } => SqlError::TypeMismatch {
        column,
        expected,
        found,
      },
      ValueError::OutOfRange(message) => SqlError::ValueOutOfRange { column, message },
      ValueError::Invalid(message) => SqlError::Decode(format!("column `{column}`: {message}")),
    }
  }
}

/// The [`SqlError`] of a sqlx failure, the same for every backend: pool exhaustion is
/// [`NotConnected`](SqlError::NotConnected), a network failure
/// [`ConnectionFailed`](SqlError::ConnectionFailed), a TLS failure
/// [`TlsFailed`](SqlError::TlsFailed) (rustls reports a failed handshake as an `InvalidData`
/// I/O error), and anything else [`QueryFailed`](SqlError::QueryFailed) with an empty `sql`. A
/// database error's message ends with its SQLSTATE: [`SQLSTATE_UNIQUE_VIOLATION`] for a unique
/// violation, else whatever `sqlstate` (the backend's code mapping) returns.
#[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
pub(crate) fn from_sqlx(
  err: sqlx::Error,
  sqlstate: impl FnOnce(&dyn sqlx::error::DatabaseError) -> Option<String>,
) -> SqlError {
  match err {
    sqlx::Error::PoolClosed | sqlx::Error::PoolTimedOut => SqlError::NotConnected,
    sqlx::Error::Configuration(msg) => SqlError::Unsupported(msg.to_string()),
    sqlx::Error::Tls(_) => SqlError::tls_failed(err.to_string()),
    sqlx::Error::Io(ref io) if io.kind() == std::io::ErrorKind::InvalidData => {
      SqlError::tls_failed(err.to_string())
    }
    sqlx::Error::Io(_) => SqlError::connection_failed(err.to_string()),
    other => {
      let code = other.as_database_error().and_then(|db| {
        if db.is_unique_violation() {
          Some(SQLSTATE_UNIQUE_VIOLATION.to_owned())
        } else {
          sqlstate(db)
        }
      });
      let message = other.to_string();
      SqlError::QueryFailed {
        sql: String::new(),
        message: match code {
          Some(code) => crate::transaction::with_sqlstate(message, &code),
          None => message,
        },
      }
    }
  }
}

impl fmt::Display for SqlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      SqlError::NotConnected => write!(f, "sql client not connected"),
      SqlError::ConnectionFailed { message } => write!(f, "sql connection failed: {message}"),
      SqlError::TlsFailed { message } => write!(f, "sql tls failed: {message}"),
      SqlError::QueryFailed { sql, message } => {
        write!(f, "sql query failed ({sql}): {message}")
      }
      SqlError::TransactionFailed(msg) => write!(f, "sql transaction failed: {msg}"),
      SqlError::Decode(msg) => write!(f, "sql decode error: {msg}"),
      SqlError::Unsupported(msg) => write!(f, "sql unsupported: {msg}"),
      SqlError::ColumnNotFound { column } => write!(f, "sql column `{column}` not found"),
      SqlError::UnexpectedNull { column } => write!(f, "sql column `{column}` is NULL"),
      SqlError::TypeMismatch {
        column,
        expected,
        found,
      } => write!(
        f,
        "sql column `{column}`: expected {expected:?}, found {found:?}"
      ),
      SqlError::ValueOutOfRange { column, message } => {
        write!(f, "sql column `{column}` out of range: {message}")
      }
      SqlError::InvalidRow { issues } => {
        write!(f, "sql row invalid")?;
        for (i, issue) in issues.iter().enumerate() {
          let sep = if i == 0 { ": " } else { "; " };
          if issue.path.is_empty() {
            write!(f, "{sep}{}", issue.message)?;
          } else {
            write!(f, "{sep}{}: {}", issue.path, issue.message)?;
          }
        }
        Ok(())
      }
      SqlError::RowNotFound => write!(f, "sql query returned no row"),
      SqlError::StaleVersion { entity, id } => write!(
        f,
        "sql stale version: {entity} {id} was changed by another writer"
      ),
    }
  }
}

impl std::error::Error for SqlError {}

#[cfg(test)]
mod tests {
  use super::{SQLSTATE_UNIQUE_VIOLATION, SqlError};
  use crate::value::{SqlType, ValueError};

  fn failed(message: &str) -> SqlError {
    SqlError::QueryFailed {
      sql: "INSERT ..".into(),
      message: message.into(),
    }
  }

  #[test]
  fn sqlstate_is_read_from_server_errors_only() {
    let dup = failed("duplicate key value violates unique constraint (SQLSTATE 23505)");
    assert_eq!(dup.sqlstate(), Some(SQLSTATE_UNIQUE_VIOLATION));
    assert!(dup.is_unique_violation());
    let tx = SqlError::TransactionFailed("could not serialize (SQLSTATE 40001)".into());
    assert_eq!(tx.sqlstate(), Some("40001"));
    assert!(!tx.is_unique_violation());
    for no_code in [
      failed("relation does not exist"),
      failed("odd (SQLSTATE nope!)"),
      failed("(SQLSTATE 23505) is not at the end"),
    ] {
      assert_eq!(no_code.sqlstate(), None, "{no_code}");
    }
    assert_eq!(SqlError::Decode("(SQLSTATE 23505)".into()).sqlstate(), None);
    assert_eq!(SqlError::tls_failed("(SQLSTATE 23505)").sqlstate(), None);
  }

  #[test]
  fn connection_and_tls_failures_display_their_cause() {
    assert_eq!(
      SqlError::connection_failed("refused").to_string(),
      "sql connection failed: refused"
    );
    assert_eq!(
      SqlError::tls_failed("server does not support TLS").to_string(),
      "sql tls failed: server does not support TLS"
    );
  }

  #[test]
  fn display_includes_query_context() {
    let err = SqlError::QueryFailed {
      sql: "SELECT 1".into(),
      message: "boom".into(),
    };
    assert!(err.to_string().contains("SELECT 1"));
    assert!(err.to_string().contains("boom"));
  }

  #[test]
  fn display_all_variants() {
    assert_eq!(
      SqlError::NotConnected.to_string(),
      "sql client not connected"
    );
    assert!(
      SqlError::TransactionFailed("tx".into())
        .to_string()
        .contains("tx")
    );
    assert!(SqlError::Decode("col".into()).to_string().contains("col"));
    assert!(
      SqlError::Unsupported("nope".into())
        .to_string()
        .contains("nope")
    );
  }

  #[test]
  fn value_errors_name_the_column() {
    let cases = [
      (ValueError::UnexpectedNull, "sql column `id` is NULL"),
      (
        ValueError::Mismatch {
          expected: SqlType::I64,
          found: SqlType::Text,
        },
        "sql column `id`: expected I64, found Text",
      ),
      (
        ValueError::OutOfRange("too big".into()),
        "sql column `id` out of range: too big",
      ),
      (
        ValueError::Invalid("bad uuid".into()),
        "sql decode error: column `id`: bad uuid",
      ),
    ];
    for (err, shown) in cases {
      assert_eq!(SqlError::from_value_error("id", err).to_string(), shown);
    }
    assert_eq!(
      SqlError::ColumnNotFound {
        column: "#3".into()
      }
      .to_string(),
      "sql column `#3` not found"
    );
  }

  #[test]
  fn display_row_level_variants() {
    use id_effect::schema::ParseError;

    let invalid = SqlError::InvalidRow {
      issues: vec![
        ParseError::new("", "expected object"),
        ParseError::new("age", "expected i64"),
      ],
    };
    assert_eq!(
      invalid.to_string(),
      "sql row invalid: expected object; age: expected i64"
    );
    assert_eq!(
      SqlError::InvalidRow { issues: vec![] }.to_string(),
      "sql row invalid"
    );
    assert_eq!(
      SqlError::RowNotFound.to_string(),
      "sql query returned no row"
    );
    assert_eq!(
      SqlError::StaleVersion {
        entity: "Todo".into(),
        id: "7".into(),
      }
      .to_string(),
      "sql stale version: Todo 7 was changed by another writer"
    );
  }
}
