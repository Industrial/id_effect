//! Map sqlx failures into [`SqlError`](crate::SqlError).

use crate::SqlError;

/// Driver-local error wrapper (mostly for pool construction).
#[derive(Debug)]
pub struct PgSqlError(pub SqlError);

impl PgSqlError {
  #[inline]
  pub(crate) fn from_sqlx(err: sqlx::Error) -> Self {
    Self(sqlx_error(err))
  }
}

/// The [`SqlError`] of a sqlx failure ([`crate::error::from_sqlx`]): a server error's message
/// ends with its SQLSTATE (`"… (SQLSTATE 23505)"`), which [`SqlError::sqlstate`] reads.
pub(crate) fn sqlx_error(err: sqlx::Error) -> SqlError {
  crate::error::from_sqlx(err, |db| db.code().map(|code| code.into_owned()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::SqlError;

  #[test]
  fn pool_closed_maps_to_not_connected() {
    let err = sqlx_error(sqlx::Error::PoolClosed);
    assert!(matches!(err, SqlError::NotConnected));
  }

  #[test]
  fn transport_failures_map_to_their_variants() {
    let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
    assert!(matches!(
      sqlx_error(sqlx::Error::Io(refused)),
      SqlError::ConnectionFailed { .. }
    ));
    let handshake =
      std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid peer certificate");
    assert!(matches!(
      sqlx_error(sqlx::Error::Io(handshake)),
      SqlError::TlsFailed { message, .. } if message.contains("invalid peer certificate")
    ));
    assert!(matches!(
      sqlx_error(sqlx::Error::Tls("server does not support TLS".into())),
      SqlError::TlsFailed { message, .. } if message.contains("server does not support TLS")
    ));
    assert!(matches!(
      sqlx_error(sqlx::Error::RowNotFound),
      SqlError::QueryFailed { message, .. } if !message.contains("SQLSTATE")
    ));
  }

  /// A server error as sqlx reports it, without a live server.
  #[derive(Debug)]
  struct FakeDbError {
    code: Option<&'static str>,
    unique: bool,
  }

  impl std::fmt::Display for FakeDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.write_str("fake server error")
    }
  }

  impl std::error::Error for FakeDbError {}

  impl sqlx::error::DatabaseError for FakeDbError {
    fn message(&self) -> &str {
      "fake server error"
    }

    fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
      self.code.map(std::borrow::Cow::Borrowed)
    }

    fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
      self
    }

    fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
      self
    }

    fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
      self
    }

    fn kind(&self) -> sqlx::error::ErrorKind {
      if self.unique {
        sqlx::error::ErrorKind::UniqueViolation
      } else {
        sqlx::error::ErrorKind::Other
      }
    }
  }

  fn server_error(code: Option<&'static str>, unique: bool) -> SqlError {
    sqlx_error(sqlx::Error::Database(Box::new(FakeDbError {
      code,
      unique,
    })))
  }

  #[test]
  fn server_errors_carry_their_sqlstate() {
    let unique = server_error(Some("23505"), true);
    assert!(unique.is_unique_violation(), "{unique}");
    // A unique violation reports 23505 whatever the backend's own code.
    assert_eq!(server_error(None, true).sqlstate(), Some("23505"));
    let serialization = server_error(Some("40001"), false);
    assert_eq!(serialization.sqlstate(), Some("40001"));
    assert!(
      matches!(&serialization, SqlError::QueryFailed { sql, message }
        if sql.is_empty() && message.contains("fake server error")),
      "{serialization:?}"
    );
    assert_eq!(server_error(None, false).sqlstate(), None);
  }

  #[test]
  fn pool_and_configuration_failures_map_to_their_variants() {
    assert!(matches!(
      PgSqlError::from_sqlx(sqlx::Error::PoolTimedOut).0,
      SqlError::NotConnected
    ));
    assert!(matches!(
      sqlx_error(sqlx::Error::Configuration("bad url".into())),
      SqlError::Unsupported(message) if message.contains("bad url")
    ));
  }

  #[test]
  fn fake_database_error_behaves_like_an_error() {
    use sqlx::error::DatabaseError as _;
    let mut fake = FakeDbError {
      code: None,
      unique: false,
    };
    assert_eq!(fake.message(), "fake server error");
    assert_eq!(fake.as_error().to_string(), "fake server error");
    assert_eq!(fake.as_error_mut().to_string(), "fake server error");
    assert_eq!(Box::new(fake).into_error().to_string(), "fake server error");
  }
}
