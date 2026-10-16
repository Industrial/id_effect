//! Pool configuration and construction via sqlx.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};

use super::error::PgSqlError;

/// Whether and how a connection negotiates TLS (libpq's `sslmode`).
///
/// Only [`VerifyFull`](Self::VerifyFull) protects against an active attacker on the network:
/// [`Prefer`](Self::Prefer) and [`Require`](Self::Require) accept any server certificate, and
/// [`VerifyCa`](Self::VerifyCa) accepts a certificate a trusted CA issued to another host. Use
/// `VerifyFull` in production (`Require` only on a network you trust).
///
/// Certificates are checked against the Mozilla root store plus the root certificate(s) of
/// [`PgPoolConfig::root_cert`]. A unix-socket connection (`?host=/run/postgresql`) never uses
/// TLS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PgTlsMode {
  /// Plaintext only (`sslmode=disable`; sqlx treats `allow` the same way).
  Disable,
  /// TLS when the server offers it, without checking its certificate, and plaintext when it does
  /// not (`sslmode=prefer`, the default). An attacker can strip TLS or impersonate the server.
  Prefer,
  /// TLS or no connection, without checking the server's certificate (`sslmode=require`; unlike
  /// libpq, a root certificate does not make this `VerifyCa`).
  Require,
  /// TLS with a certificate issued by a trusted CA, for any host name (`sslmode=verify-ca`).
  ///
  /// With sqlx 0.8 and rustls 0.23 the host name is in fact checked too (sqlx misses rustls'
  /// newer name-mismatch error), so a certificate for another host fails as under
  /// [`VerifyFull`](Self::VerifyFull).
  VerifyCa,
  /// TLS with a certificate issued by a trusted CA for the host connected to
  /// (`sslmode=verify-full`).
  VerifyFull,
}

impl PgTlsMode {
  fn to_sqlx(self) -> PgSslMode {
    match self {
      PgTlsMode::Disable => PgSslMode::Disable,
      PgTlsMode::Prefer => PgSslMode::Prefer,
      PgTlsMode::Require => PgSslMode::Require,
      PgTlsMode::VerifyCa => PgSslMode::VerifyCa,
      PgTlsMode::VerifyFull => PgSslMode::VerifyFull,
    }
  }

  fn from_sqlx(mode: PgSslMode) -> Self {
    match mode {
      // sqlx 0.8 never attempts TLS under `allow`.
      PgSslMode::Disable | PgSslMode::Allow => PgTlsMode::Disable,
      PgSslMode::Prefer => PgTlsMode::Prefer,
      PgSslMode::Require => PgTlsMode::Require,
      PgSslMode::VerifyCa => PgTlsMode::VerifyCa,
      PgSslMode::VerifyFull => PgTlsMode::VerifyFull,
    }
  }
}

/// CA certificate(s) a server certificate may chain to (libpq's `sslrootcert`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PgRootCert {
  /// A PEM file, read each time a connection is opened.
  File(PathBuf),
  /// PEM-encoded certificates.
  Pem(Vec<u8>),
}

/// sqlx PostgreSQL pool sizing, connection string and TLS settings.
///
/// TLS comes from the URL (`?sslmode=verify-full&sslrootcert=/etc/ssl/db-ca.pem`) or the
/// `PGSSLMODE` / `PGSSLROOTCERT` environment variables, unless [`tls_mode`](Self::tls_mode) /
/// [`root_cert`](Self::root_cert) are set, which override both. With none of them the mode is
/// [`PgTlsMode::Prefer`]: **opportunistic TLS that a network attacker can downgrade to plaintext
/// or intercept**. Production deployments should use [`PgTlsMode::VerifyFull`].
#[derive(Clone, Debug)]
pub struct PgPoolConfig {
  /// PostgreSQL connection URL (`postgres://…`).
  pub url: String,
  /// Maximum pooled connections.
  pub max_size: u32,
  /// Pool acquire timeout (default 2s).
  pub acquire_timeout: Duration,
  /// TLS mode overriding the URL's `sslmode` (`None`: the URL's, else `PGSSLMODE`, else
  /// [`PgTlsMode::Prefer`]).
  pub tls_mode: Option<PgTlsMode>,
  /// Root certificate(s) overriding the URL's `sslrootcert` (`None`: the URL's, else
  /// `PGSSLROOTCERT`). Only [`PgTlsMode::VerifyCa`] and [`PgTlsMode::VerifyFull`] use them.
  pub root_cert: Option<PgRootCert>,
}

impl PgPoolConfig {
  /// Build config from a connection URL with default pool size (16).
  #[inline]
  pub fn from_url(url: impl Into<String>) -> Self {
    Self {
      url: url.into(),
      max_size: 16,
      acquire_timeout: Duration::from_secs(2),
      tls_mode: None,
      root_cert: None,
    }
  }

  /// Override maximum pool size.
  #[inline]
  pub fn with_max_size(mut self, max_size: u32) -> Self {
    self.max_size = max_size;
    self
  }

  /// Set the TLS mode, overriding the URL's `sslmode`.
  #[inline]
  #[must_use]
  pub fn with_tls_mode(mut self, mode: PgTlsMode) -> Self {
    self.tls_mode = Some(mode);
    self
  }

  /// Trust the CA certificate(s) in the PEM file at `path`, overriding the URL's `sslrootcert`.
  #[inline]
  #[must_use]
  pub fn with_root_cert_file(mut self, path: impl Into<PathBuf>) -> Self {
    self.root_cert = Some(PgRootCert::File(path.into()));
    self
  }

  /// Trust the PEM-encoded CA certificate(s) `pem`, overriding the URL's `sslrootcert`.
  #[inline]
  #[must_use]
  pub fn with_root_cert_pem(mut self, pem: impl Into<Vec<u8>>) -> Self {
    self.root_cert = Some(PgRootCert::Pem(pem.into()));
    self
  }

  /// The TLS mode connections use: [`tls_mode`](Self::tls_mode), else the URL's `sslmode`, else
  /// `PGSSLMODE`, else [`PgTlsMode::Prefer`]. Fails like [`pg_pool_from_config`] on an invalid
  /// URL.
  pub fn effective_tls_mode(&self) -> Result<PgTlsMode, PgSqlError> {
    Ok(PgTlsMode::from_sqlx(self.connect_options()?.get_ssl_mode()))
  }

  /// The URL's options with the explicit TLS settings applied over them.
  fn connect_options(&self) -> Result<PgConnectOptions, PgSqlError> {
    let mut options = PgConnectOptions::from_str(&self.url).map_err(PgSqlError::from_sqlx)?;
    if let Some(mode) = self.tls_mode {
      options = options.ssl_mode(mode.to_sqlx());
    }
    match &self.root_cert {
      Some(PgRootCert::File(path)) => options = options.ssl_root_cert(path),
      Some(PgRootCert::Pem(pem)) => options = options.ssl_root_cert_from_pem(pem.clone()),
      None => {}
    }
    Ok(options)
  }

  fn options(&self) -> PgPoolOptions {
    PgPoolOptions::new()
      .max_connections(self.max_size)
      .acquire_timeout(self.acquire_timeout)
      .idle_timeout(None)
  }
}

/// Create a [`PgPool`] from `config` (connects eagerly). A TLS failure (the server refuses TLS
/// under [`PgTlsMode::Require`] or stricter, or its certificate does not verify) is
/// [`SqlError::QueryFailed`](crate::SqlError::QueryFailed) with an empty `sql` and the cause as
/// its message.
pub async fn pg_pool_from_config(config: PgPoolConfig) -> Result<PgPool, PgSqlError> {
  let options = config.connect_options()?;
  config
    .options()
    .connect_with(options)
    .await
    .map_err(PgSqlError::from_sqlx)
}

/// Lazy pool for unit tests and provider wiring without immediate connect.
pub fn pg_pool_from_config_lazy(config: PgPoolConfig) -> Result<PgPool, PgSqlError> {
  let options = config.connect_options()?;
  Ok(config.options().connect_lazy_with(options))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::SqlError;

  const MODES: [PgTlsMode; 5] = [
    PgTlsMode::Disable,
    PgTlsMode::Prefer,
    PgTlsMode::Require,
    PgTlsMode::VerifyCa,
    PgTlsMode::VerifyFull,
  ];

  #[test]
  fn modes_round_trip_through_sqlx() {
    for mode in MODES {
      assert_eq!(PgTlsMode::from_sqlx(mode.to_sqlx()), mode);
    }
    assert_eq!(PgTlsMode::from_sqlx(PgSslMode::Allow), PgTlsMode::Disable);
  }

  #[test]
  fn the_config_overrides_the_url_mode() {
    let config =
      PgPoolConfig::from_url("postgres://u@db/app?sslmode=verify-full&sslrootcert=/etc/ca.pem");
    assert_eq!(config.effective_tls_mode().unwrap(), PgTlsMode::VerifyFull);
    for mode in MODES {
      let config = config.clone().with_tls_mode(mode);
      assert_eq!(config.effective_tls_mode().unwrap(), mode);
    }
  }

  #[test]
  fn root_certs_are_recorded() {
    let file = PgPoolConfig::from_url("postgres://db/app").with_root_cert_file("/etc/ca.pem");
    assert_eq!(file.root_cert, Some(PgRootCert::File("/etc/ca.pem".into())));
    assert!(file.connect_options().is_ok());
    let pem = file.with_root_cert_pem(b"-----BEGIN CERTIFICATE-----".to_vec());
    assert_eq!(
      pem.root_cert,
      Some(PgRootCert::Pem(b"-----BEGIN CERTIFICATE-----".to_vec()))
    );
    assert!(pem.connect_options().is_ok());
  }

  #[test]
  fn an_unknown_sslmode_is_unsupported() {
    let bad = PgPoolConfig::from_url("postgres://db/app?sslmode=sometimes");
    let err = bad.effective_tls_mode().unwrap_err();
    assert!(matches!(err.0, SqlError::Unsupported(_)), "{:?}", err.0);
  }
}
