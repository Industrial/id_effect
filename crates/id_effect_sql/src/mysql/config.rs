//! Pool configuration and construction via sqlx.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use sqlx::MySqlPool;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlSslMode};

use super::error::MySqlSqlError;

/// The session time zone every connection is set to, so `TIMESTAMP` values are read and written
/// in UTC.
const UTC: &str = "+00:00";

/// Whether and how a connection negotiates TLS (the MySQL client's `--ssl-mode`).
///
/// Only [`VerifyIdentity`](Self::VerifyIdentity) protects against an active attacker on the
/// network: [`Preferred`](Self::Preferred) and [`Required`](Self::Required) accept any server
/// certificate, and [`VerifyCa`](Self::VerifyCa) accepts a certificate a trusted CA issued to
/// another host. Use `VerifyIdentity` in production (`Required` only on a network you trust).
///
/// Certificates are checked against the Mozilla root store plus the CA certificate(s) of
/// [`MySqlPoolConfig::root_cert`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MySqlTlsMode {
  /// Plaintext only (`ssl-mode=DISABLED`).
  Disabled,
  /// TLS when the server offers it, without checking its certificate, and plaintext when it does
  /// not (`ssl-mode=PREFERRED`, the default). An attacker can strip TLS or impersonate the
  /// server.
  Preferred,
  /// TLS or no connection, without checking the server's certificate (`ssl-mode=REQUIRED`).
  Required,
  /// TLS with a certificate issued by a trusted CA, for any host name (`ssl-mode=VERIFY_CA`).
  ///
  /// With sqlx 0.8 and rustls 0.23 the host name is in fact checked too (sqlx misses rustls'
  /// newer name-mismatch error), so a certificate for another host fails as under
  /// [`VerifyIdentity`](Self::VerifyIdentity).
  VerifyCa,
  /// TLS with a certificate issued by a trusted CA for the host connected to
  /// (`ssl-mode=VERIFY_IDENTITY`).
  VerifyIdentity,
}

impl MySqlTlsMode {
  fn to_sqlx(self) -> MySqlSslMode {
    match self {
      MySqlTlsMode::Disabled => MySqlSslMode::Disabled,
      MySqlTlsMode::Preferred => MySqlSslMode::Preferred,
      MySqlTlsMode::Required => MySqlSslMode::Required,
      MySqlTlsMode::VerifyCa => MySqlSslMode::VerifyCa,
      MySqlTlsMode::VerifyIdentity => MySqlSslMode::VerifyIdentity,
    }
  }

  fn from_sqlx(mode: MySqlSslMode) -> Self {
    match mode {
      MySqlSslMode::Disabled => MySqlTlsMode::Disabled,
      MySqlSslMode::Preferred => MySqlTlsMode::Preferred,
      MySqlSslMode::Required => MySqlTlsMode::Required,
      MySqlSslMode::VerifyCa => MySqlTlsMode::VerifyCa,
      MySqlSslMode::VerifyIdentity => MySqlTlsMode::VerifyIdentity,
    }
  }
}

/// CA certificate(s) a server certificate may chain to (the MySQL client's `--ssl-ca`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MySqlRootCert {
  /// A PEM file, read each time a connection is opened.
  File(PathBuf),
  /// PEM-encoded certificates.
  Pem(Vec<u8>),
}

/// sqlx MySQL pool sizing, connection string and TLS settings.
///
/// TLS comes from the URL (`?ssl-mode=VERIFY_IDENTITY&ssl-ca=/etc/ssl/db-ca.pem`) unless
/// [`tls_mode`](Self::tls_mode) / [`root_cert`](Self::root_cert) are set, which override it.
/// With neither, the mode is [`MySqlTlsMode::Preferred`]: **opportunistic TLS that a network
/// attacker can downgrade to plaintext or intercept**. Production deployments should use
/// [`MySqlTlsMode::VerifyIdentity`].
#[derive(Clone, Debug)]
pub struct MySqlPoolConfig {
  /// MySQL connection URL (`mysql://user:password@host:3306/database`, with sqlx's query
  /// parameters: `?ssl-mode=…`, `?socket=…`, …).
  pub url: String,
  /// Maximum pooled connections.
  pub max_size: u32,
  /// Pool acquire timeout, which includes opening a connection (default 5s).
  pub acquire_timeout: Duration,
  /// TLS mode overriding the URL's `ssl-mode` (`None`: the URL's, else
  /// [`MySqlTlsMode::Preferred`]).
  pub tls_mode: Option<MySqlTlsMode>,
  /// CA certificate(s) overriding the URL's `ssl-ca`. Only [`MySqlTlsMode::VerifyCa`] and
  /// [`MySqlTlsMode::VerifyIdentity`] use them.
  pub root_cert: Option<MySqlRootCert>,
}

impl MySqlPoolConfig {
  /// Config for a connection URL, with a pool of up to 16 connections.
  #[inline]
  pub fn from_url(url: impl Into<String>) -> Self {
    Self {
      url: url.into(),
      max_size: 16,
      acquire_timeout: Duration::from_secs(5),
      tls_mode: None,
      root_cert: None,
    }
  }

  /// Override maximum pool size.
  #[inline]
  #[must_use]
  pub fn with_max_size(mut self, max_size: u32) -> Self {
    self.max_size = max_size;
    self
  }

  /// Set the TLS mode, overriding the URL's `ssl-mode`.
  #[inline]
  #[must_use]
  pub fn with_tls_mode(mut self, mode: MySqlTlsMode) -> Self {
    self.tls_mode = Some(mode);
    self
  }

  /// Trust the CA certificate(s) in the PEM file at `path`, overriding the URL's `ssl-ca`.
  #[inline]
  #[must_use]
  pub fn with_root_cert_file(mut self, path: impl Into<PathBuf>) -> Self {
    self.root_cert = Some(MySqlRootCert::File(path.into()));
    self
  }

  /// Trust the PEM-encoded CA certificate(s) `pem`, overriding the URL's `ssl-ca`.
  #[inline]
  #[must_use]
  pub fn with_root_cert_pem(mut self, pem: impl Into<Vec<u8>>) -> Self {
    self.root_cert = Some(MySqlRootCert::Pem(pem.into()));
    self
  }

  /// The TLS mode connections use: [`tls_mode`](Self::tls_mode), else the URL's `ssl-mode`, else
  /// [`MySqlTlsMode::Preferred`]. Fails like [`mysql_pool_from_config`] on an invalid URL.
  pub fn effective_tls_mode(&self) -> Result<MySqlTlsMode, MySqlSqlError> {
    Ok(MySqlTlsMode::from_sqlx(
      self.connect_options()?.get_ssl_mode(),
    ))
  }

  /// The URL's options with the explicit TLS settings applied over them, and the session time
  /// zone forced to UTC.
  fn connect_options(&self) -> Result<MySqlConnectOptions, MySqlSqlError> {
    let mut options = MySqlConnectOptions::from_str(&self.url)
      .map_err(MySqlSqlError::from_sqlx)?
      .timezone(Some(UTC.to_owned()));
    if let Some(mode) = self.tls_mode {
      options = options.ssl_mode(mode.to_sqlx());
    }
    match &self.root_cert {
      Some(MySqlRootCert::File(path)) => options = options.ssl_ca(path),
      Some(MySqlRootCert::Pem(pem)) => options = options.ssl_ca_from_pem(pem.clone()),
      None => {}
    }
    Ok(options)
  }

  fn pool_options(&self) -> MySqlPoolOptions {
    MySqlPoolOptions::new()
      .max_connections(self.max_size)
      .acquire_timeout(self.acquire_timeout)
      .idle_timeout(None)
  }
}

/// Create a [`MySqlPool`] from `config` (opens a first connection). A TLS failure (the server
/// offers no TLS under [`MySqlTlsMode::Required`] or stricter, or its certificate does not
/// verify) is [`SqlError::QueryFailed`](crate::SqlError::QueryFailed) with an empty `sql` and the
/// cause as its message.
pub async fn mysql_pool_from_config(config: MySqlPoolConfig) -> Result<MySqlPool, MySqlSqlError> {
  let options = config.connect_options()?;
  config
    .pool_options()
    .connect_with(options)
    .await
    .map_err(MySqlSqlError::from_sqlx)
}

/// A [`MySqlPool`] that opens connections on first use.
pub fn mysql_pool_from_config_lazy(config: MySqlPoolConfig) -> Result<MySqlPool, MySqlSqlError> {
  let options = config.connect_options()?;
  Ok(config.pool_options().connect_lazy_with(options))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::SqlError;

  #[test]
  fn builders_set_their_fields() {
    let config = MySqlPoolConfig::from_url("mysql://u:p@db:3307/app").with_max_size(3);
    assert_eq!(
      (config.url.as_str(), config.max_size, config.acquire_timeout),
      ("mysql://u:p@db:3307/app", 3, Duration::from_secs(5))
    );
    let options = config.connect_options().expect("options");
    assert_eq!(
      (
        options.get_host(),
        options.get_port(),
        options.get_database()
      ),
      ("db", 3307, Some("app"))
    );
  }

  const MODES: [MySqlTlsMode; 5] = [
    MySqlTlsMode::Disabled,
    MySqlTlsMode::Preferred,
    MySqlTlsMode::Required,
    MySqlTlsMode::VerifyCa,
    MySqlTlsMode::VerifyIdentity,
  ];

  #[test]
  fn the_config_overrides_the_url_mode() {
    let default = MySqlPoolConfig::from_url("mysql://u@db/app");
    assert_eq!(
      default.effective_tls_mode().unwrap(),
      MySqlTlsMode::Preferred
    );
    let config = MySqlPoolConfig::from_url("mysql://u@db/app?ssl-mode=VERIFY_IDENTITY&ssl-ca=/ca");
    assert_eq!(
      config.effective_tls_mode().unwrap(),
      MySqlTlsMode::VerifyIdentity
    );
    for mode in MODES {
      let config = config.clone().with_tls_mode(mode);
      assert_eq!(config.effective_tls_mode().unwrap(), mode);
      assert_eq!(MySqlTlsMode::from_sqlx(mode.to_sqlx()), mode);
    }
  }

  #[test]
  fn root_certs_are_recorded() {
    let file = MySqlPoolConfig::from_url("mysql://db/app").with_root_cert_file("/etc/ca.pem");
    assert_eq!(
      file.root_cert,
      Some(MySqlRootCert::File("/etc/ca.pem".into()))
    );
    assert!(file.connect_options().is_ok());
    let pem = file.with_root_cert_pem(b"-----BEGIN CERTIFICATE-----".to_vec());
    assert_eq!(
      pem.root_cert,
      Some(MySqlRootCert::Pem(b"-----BEGIN CERTIFICATE-----".to_vec()))
    );
    assert!(pem.connect_options().is_ok());
  }

  #[test]
  fn invalid_urls_are_unsupported() {
    let err = mysql_pool_from_config_lazy(MySqlPoolConfig::from_url("mysql://h/db?ssl-mode=nope"))
      .unwrap_err();
    assert!(matches!(err.0, SqlError::Unsupported(_)), "{:?}", err.0);
  }
}
