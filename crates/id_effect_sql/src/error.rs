//! Structured errors for SQL client operations.

use std::fmt;

use id_effect::schema::ParseError;

use crate::value::{SqlType, ValueError};

/// SQL client / query failures.
///
/// Non-exhaustive: match it with a wildcard arm. A `column` field names the column, or `#3` for
/// an index into an unnamed column (`$2` for a bound parameter).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SqlError {
  /// Client is not connected (or pool unavailable).
  NotConnected,
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

impl fmt::Display for SqlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      SqlError::NotConnected => write!(f, "sql client not connected"),
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
  use super::SqlError;
  use crate::value::{SqlType, ValueError};

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
}
