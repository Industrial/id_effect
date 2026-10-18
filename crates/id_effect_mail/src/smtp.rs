//! [`SmtpMailer`]: a [`Mailer`] over lettre's pooled async SMTP transport (tokio, rustls with
//! aws-lc-rs and the webpki roots; release spec 0.10.0 D9).
//!
//! [`SmtpConfig`] picks the connection security: implicit TLS ([`SmtpConfig::tls`], port 465),
//! a required `STARTTLS` upgrade ([`SmtpConfig::starttls`], port 587), or plaintext for a local
//! relay or a development catcher such as Mailpit ([`SmtpConfig::plaintext`]). Connections are
//! pooled and reused across sends; the pool is created on the first send, on the running tokio
//! runtime. The password never appears in `Debug` output.
//!
//! A send is not retried here: an SMTP `4xx` reply is [`MailError::Temporary`], so a caller that
//! wants retries (such as a job) decides on them.

use std::fmt;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use id_effect::{Effect, Layer, fail, from_async, succeed};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use lettre::transport::smtp::{AsyncSmtpTransportBuilder, PoolConfig};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::{MailError, Mailer, MailerEnv, Message, Sent, mailer_env};

/// How an SMTP connection is secured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpSecurity {
  /// TLS from the first byte (SMTPS, default port 465).
  Tls,
  /// Plaintext, upgraded with a required `STARTTLS` before anything is sent (default port 587).
  StartTls,
  /// Never encrypted (default port 25). Only for a relay on a trusted network or a local
  /// development catcher.
  Plaintext,
}

impl SmtpSecurity {
  /// The port used when none is configured.
  pub fn default_port(self) -> u16 {
    match self {
      SmtpSecurity::Tls => 465,
      SmtpSecurity::StartTls => 587,
      SmtpSecurity::Plaintext => 25,
    }
  }
}

/// Connection settings for [`SmtpMailer`].
#[derive(Clone)]
pub struct SmtpConfig {
  host: String,
  port: Option<u16>,
  security: SmtpSecurity,
  username: Option<String>,
  password: Option<String>,
  timeout: Duration,
  pool_max_size: u32,
  pool_min_idle: u32,
  pool_idle_timeout: Duration,
  hello_name: Option<String>,
}

impl SmtpConfig {
  /// Default timeout of each SMTP command (30 s).
  pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
  /// Default most pooled connections (10).
  pub const DEFAULT_POOL_MAX_SIZE: u32 = 10;
  /// Default time an unused pooled connection stays open (60 s).
  pub const DEFAULT_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

  fn with_security(host: impl Into<String>, security: SmtpSecurity) -> Self {
    Self {
      host: host.into(),
      port: None,
      security,
      username: None,
      password: None,
      timeout: Self::DEFAULT_TIMEOUT,
      pool_max_size: Self::DEFAULT_POOL_MAX_SIZE,
      pool_min_idle: 0,
      pool_idle_timeout: Self::DEFAULT_POOL_IDLE_TIMEOUT,
      hello_name: None,
    }
  }

  /// Implicit TLS to `host` on port 465; the certificate must be valid for `host`.
  pub fn tls(host: impl Into<String>) -> Self {
    Self::with_security(host, SmtpSecurity::Tls)
  }

  /// `STARTTLS` (required) to `host` on port 587; the certificate must be valid for `host`.
  pub fn starttls(host: impl Into<String>) -> Self {
    Self::with_security(host, SmtpSecurity::StartTls)
  }

  /// Unencrypted SMTP to `host:port` (a local relay, Mailpit on 1025).
  pub fn plaintext(host: impl Into<String>, port: u16) -> Self {
    Self::with_security(host, SmtpSecurity::Plaintext).port(port)
  }

  /// Replaces the port.
  pub fn port(mut self, port: u16) -> Self {
    self.port = Some(port);
    self
  }

  /// Authenticates as `username` with `password` (kept out of `Debug`).
  pub fn credentials(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
    self.username = Some(username.into());
    self.password = Some(password.into());
    self
  }

  /// Replaces the per-command timeout.
  pub fn timeout(mut self, timeout: Duration) -> Self {
    self.timeout = timeout;
    self
  }

  /// Replaces the most pooled connections (at least 1).
  pub fn pool_max_size(mut self, size: u32) -> Self {
    self.pool_max_size = size.max(1);
    self
  }

  /// Keeps at least `count` connections open once the pool runs (default 0).
  pub fn pool_min_idle(mut self, count: u32) -> Self {
    self.pool_min_idle = count;
    self
  }

  /// Replaces how long an unused pooled connection stays open.
  pub fn pool_idle_timeout(mut self, timeout: Duration) -> Self {
    self.pool_idle_timeout = timeout;
    self
  }

  /// The name sent with `EHLO` (default the address literal `[127.0.0.1]`; set the host's
  /// public name for relays that check it).
  pub fn hello_name(mut self, name: impl Into<String>) -> Self {
    self.hello_name = Some(name.into());
    self
  }

  /// The server host.
  pub fn host(&self) -> &str {
    &self.host
  }

  /// The port connected to: the configured one or [`SmtpSecurity::default_port`].
  pub fn effective_port(&self) -> u16 {
    self.port.unwrap_or_else(|| self.security.default_port())
  }

  /// The connection security.
  pub fn security(&self) -> SmtpSecurity {
    self.security
  }

  /// The user name, when authenticating.
  pub fn username(&self) -> Option<&str> {
    self.username.as_deref()
  }

  fn transport_builder(&self) -> Result<AsyncSmtpTransportBuilder, MailError> {
    if self.host.trim().is_empty() {
      return Err(MailError::Config("the SMTP host is empty".into()));
    }
    let tls_for = |host: &str| {
      TlsParameters::new(host.to_owned())
        .map_err(|e| MailError::Config(format!("TLS for {host}: {e}")))
    };
    let tls = match self.security {
      SmtpSecurity::Tls => Tls::Wrapper(tls_for(&self.host)?),
      SmtpSecurity::StartTls => Tls::Required(tls_for(&self.host)?),
      SmtpSecurity::Plaintext => Tls::None,
    };
    let pool = PoolConfig::new()
      .max_size(self.pool_max_size)
      .min_idle(self.pool_min_idle)
      .idle_timeout(self.pool_idle_timeout);
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(self.host.clone())
      .port(self.effective_port())
      .tls(tls)
      .timeout(Some(self.timeout))
      .pool_config(pool);
    if let (Some(user), Some(password)) = (&self.username, &self.password) {
      builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
    }
    if let Some(name) = &self.hello_name {
      builder = builder.hello_name(ClientId::Domain(name.clone()));
    }
    Ok(builder)
  }
}

impl fmt::Debug for SmtpConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SmtpConfig")
      .field("host", &self.host)
      .field("port", &self.effective_port())
      .field("security", &self.security)
      .field("username", &self.username)
      .field("password", &self.password.as_ref().map(|_| "<redacted>"))
      .field("timeout", &self.timeout)
      .field("pool_max_size", &self.pool_max_size)
      .field("pool_min_idle", &self.pool_min_idle)
      .field("pool_idle_timeout", &self.pool_idle_timeout)
      .field("hello_name", &self.hello_name)
      .finish()
  }
}

/// A [`Mailer`] over a pooled SMTP connection. Cloning shares the pool.
#[derive(Clone)]
pub struct SmtpMailer {
  inner: Arc<Inner>,
}

struct Inner {
  config: SmtpConfig,
  builder: AsyncSmtpTransportBuilder,
  /// Built on the first send: lettre's pool spawns its idle reaper on the tokio runtime.
  transport: OnceLock<AsyncSmtpTransport<Tokio1Executor>>,
}

impl SmtpMailer {
  /// A mailer for `config`; [`MailError::Config`] for an empty host or a TLS setup failure.
  /// Nothing connects until the first send.
  pub fn new(config: SmtpConfig) -> Result<Self, MailError> {
    let builder = config.transport_builder()?;
    Ok(Self {
      inner: Arc::new(Inner {
        config,
        builder,
        transport: OnceLock::new(),
      }),
    })
  }

  /// A [`Layer`] providing an [`SmtpMailer`] for `config` as the
  /// [`MailerService`](crate::MailerService), built once per layer build.
  pub fn layer<E>(config: SmtpConfig) -> Layer<MailerEnv, E>
  where
    E: From<MailError> + Send + 'static,
  {
    Layer::effect(move || match SmtpMailer::new(config.clone()) {
      Ok(mailer) => succeed(mailer_env(mailer)),
      Err(e) => fail(E::from(e)),
    })
  }

  /// The configuration.
  pub fn config(&self) -> &SmtpConfig {
    &self.inner.config
  }
}

impl fmt::Debug for SmtpMailer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SmtpMailer")
      .field("config", &self.inner.config)
      .finish_non_exhaustive()
  }
}

impl Mailer for SmtpMailer {
  fn send(&self, message: Message) -> Effect<Sent, MailError> {
    let inner = Arc::clone(&self.inner);
    from_async(move || async move {
      let id = message.new_message_id();
      let mime = message.to_lettre(&id)?;
      let transport = inner
        .transport
        .get_or_init(|| inner.builder.clone().build());
      transport.send(mime).await.map_err(smtp_error)?;
      Ok(Sent { id: Some(id) })
    })
  }
}

/// The [`MailError`] for a lettre SMTP failure: auth codes are [`MailError::Unauthorized`],
/// `5xx` [`MailError::Rejected`], `4xx` [`MailError::Temporary`], anything without a reply code
/// [`MailError::Transport`].
fn smtp_error(e: lettre::transport::smtp::Error) -> MailError {
  let message = e.to_string();
  let code = e.status().and_then(|c| c.to_string().parse::<u16>().ok());
  match code {
    Some(530 | 534 | 535) => MailError::Unauthorized(message),
    Some(code) if e.is_permanent() => MailError::Rejected { code, message },
    Some(code) => MailError::Temporary { code, message },
    None => MailError::Transport(message),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ports_follow_the_security() {
    assert_eq!(SmtpConfig::tls("smtp.example").effective_port(), 465);
    assert_eq!(SmtpConfig::starttls("smtp.example").effective_port(), 587);
    assert_eq!(
      SmtpConfig::plaintext("localhost", 1025).effective_port(),
      1025
    );
    assert_eq!(
      SmtpConfig::starttls("smtp.example")
        .port(2525)
        .effective_port(),
      2525
    );
  }

  #[test]
  fn the_password_is_never_in_debug() {
    let config = SmtpConfig::starttls("smtp.example").credentials("apikey", "hunter2-secret");
    let shown = format!("{config:?}");
    assert!(!shown.contains("hunter2-secret"));
    assert!(shown.contains("<redacted>"));
    assert!(shown.contains("apikey"));
    let mailer = SmtpMailer::new(config).unwrap();
    assert!(!format!("{mailer:?}").contains("hunter2-secret"));
  }

  #[test]
  fn an_empty_host_is_a_config_error() {
    assert!(matches!(
      SmtpMailer::new(SmtpConfig::tls(" ")),
      Err(MailError::Config(_))
    ));
    assert!(SmtpMailer::new(SmtpConfig::tls("smtp.example")).is_ok());
  }

  #[test]
  fn the_pool_keeps_at_least_one_connection() {
    let config = SmtpConfig::plaintext("localhost", 1025).pool_max_size(0);
    assert!(format!("{config:?}").contains("pool_max_size: 1"));
  }
}
