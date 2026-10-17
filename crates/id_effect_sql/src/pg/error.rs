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
}
