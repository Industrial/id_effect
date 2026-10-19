//! TLS on TCP (feature `tls`, ADR 0014 D10): TLS 1.3 and 1.2 with ALPN `h2` and `http/1.1`,
//! the certificate from an [`id_effect_platform::tls::Identity`] (or any rustls certificate
//! resolver), read on every handshake, so a reloaded certificate reaches new connections
//! without a restart. Connections already open keep the certificate they were established with.
//!
//! [`ServerTls`] is the listener's TLS settings; serve with
//! [`serve_tls_listener`](crate::server::serve_tls_listener), or set
//! [`HostConfig::tls`](crate::server::HostConfig::tls) and every host entry point serves TLS.
//! The [`ConnectionLimits`](crate::server::ConnectionLimits) apply to TLS connections as to
//! plain ones, plus a TLS handshake timeout and, on HTTP/2, a cap on concurrent streams.
//!
//! ```rust
//! use id_effect_axum::server::tls::{ALPN_H2, ALPN_HTTP11, ServerTls};
//! use id_effect_platform::tls::{Identity, NodeCertificate};
//!
//! let identity = Identity::from_certificate(&NodeCertificate::self_signed("localhost").unwrap())
//!   .unwrap();
//! let tls = ServerTls::from_identity(&identity).unwrap();
//! let config = tls.rustls_config();
//! assert_eq!(config.alpn_protocols, vec![ALPN_H2.to_vec(), ALPN_HTTP11.to_vec()]);
//! assert_eq!(config.max_early_data_size, 0);
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use id_effect_platform::tls::{CertError, CertReloader, Identity, ReloadTask, provider};
use tokio::net::TcpStream;
use tokio_rustls::rustls;
use tokio_rustls::server::TlsStream;

/// ALPN id of HTTP/2 (RFC 7301).
pub const ALPN_H2: &[u8] = b"h2";
/// ALPN id of HTTP/1.1 (RFC 7301).
pub const ALPN_HTTP11: &[u8] = b"http/1.1";

/// The TLS settings of a TCP listener: an immutable rustls [`ServerConfig`](rustls::ServerConfig)
/// whose certificate resolver is read on every handshake. Clones share the config (and the
/// certificate polling task of [`ServerTls::from_pem_files`], which stops with the last clone).
#[derive(Clone)]
pub struct ServerTls {
  config: Arc<rustls::ServerConfig>,
  reload: Option<Arc<ReloadTask>>,
  /// The certificate HTTP/3 presents too (QUIC needs an [`Identity`], not a resolver).
  identity: Option<Identity>,
}

impl std::fmt::Debug for ServerTls {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ServerTls")
      .field("alpn", &self.config.alpn_protocols)
      .field("reloading", &self.reload.is_some())
      .field("identity", &self.identity.is_some())
      .finish_non_exhaustive()
  }
}

/// Two [`ServerTls`] are equal when they share one config (clones of each other).
impl PartialEq for ServerTls {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.config, &other.config)
  }
}

impl Eq for ServerTls {}

impl ServerTls {
  /// TLS 1.3 and 1.2, ALPN `h2` then `http/1.1`, presenting `identity` (a clone shares its
  /// slot, so a [`CertReloader`] swap is used from the next handshake on). The identity is
  /// carried for HTTP/3 too ([`Self::identity`]), so a host given these settings
  /// ([`HostTls::Provided`](crate::server::HostTls::Provided)) serves HTTP/3 with it.
  ///
  /// # Errors
  ///
  /// [`CertError::Tls`] when rustls refuses the config.
  pub fn from_identity(identity: &Identity) -> Result<Self, CertError> {
    Self::builder(Arc::new(identity.clone()))
      .identity(identity.clone())
      .build()
  }

  /// The certificate identity HTTP/3 presents with these settings: set by
  /// [`Self::from_identity`], [`Self::from_pem_files`] and [`ServerTlsBuilder::identity`];
  /// `None` for a bare resolver or a rustls config (the host then serves TCP only).
  pub fn identity(&self) -> Option<&Identity> {
    self.identity.as_ref()
  }

  /// Reads the PEM chain and key now and, when `reload_every` is set, polls the files at that
  /// interval on the current Tokio runtime ([`CertReloader::spawn_polling`]): a rotated
  /// certificate is used for new connections, a file that does not parse keeps the current one.
  /// Polling stops when the last clone of the returned value is dropped.
  ///
  /// # Errors
  ///
  /// The [`CertError`] of [`CertReloader::new`]; [`CertError::Tls`] when polling is asked for
  /// outside a Tokio runtime.
  pub fn from_pem_files(
    chain: impl Into<PathBuf>,
    key: impl Into<PathBuf>,
    reload_every: Option<Duration>,
  ) -> Result<Self, CertError> {
    let reloader = Arc::new(CertReloader::new(chain, key)?);
    let reload = match reload_every {
      Some(every) => {
        if tokio::runtime::Handle::try_current().is_err() {
          return Err(CertError::Tls(
            "certificate polling needs a Tokio runtime".to_owned(),
          ));
        }
        Some(Arc::new(reloader.spawn_polling(every)))
      }
      None => None,
    };
    let mut tls = Self::from_identity(&reloader.identity())?;
    tls.reload = reload;
    Ok(tls)
  }

  /// Settings around any certificate resolver (for instance one that also answers ACME
  /// TLS-ALPN-01 challenges); the defaults of [`Self::from_identity`] until changed.
  pub fn builder(resolver: Arc<dyn rustls::server::ResolvesServerCert>) -> ServerTlsBuilder {
    ServerTlsBuilder {
      resolver,
      tls12: true,
      extra_alpn: Vec::new(),
      hooks: Vec::new(),
      identity: None,
    }
  }

  /// A rustls config built elsewhere (say `id_effect_platform::tls::server_tls_config_with`),
  /// used as given except that early data (0-RTT) is turned off. A connection whose ALPN is
  /// neither `h2` nor `http/1.1` (nor absent) is closed after the handshake, so `acme-tls/1`
  /// challenge connections never reach the router.
  pub fn from_rustls_config(mut config: rustls::ServerConfig) -> Self {
    config.max_early_data_size = 0;
    Self {
      config: Arc::new(config),
      reload: None,
      identity: None,
    }
  }

  /// The rustls config every handshake uses.
  pub fn rustls_config(&self) -> &Arc<rustls::ServerConfig> {
    &self.config
  }

  /// Runs the handshake on `stream` within `timeout` (`None`: no limit). `None` when it
  /// failed or timed out (the socket is then dropped, which closes it).
  pub(crate) async fn accept(
    &self,
    stream: TcpStream,
    timeout: Option<Duration>,
  ) -> Option<(TlsStream<TcpStream>, Negotiated)> {
    let handshake = tokio_rustls::TlsAcceptor::from(Arc::clone(&self.config)).accept(stream);
    let stream = match timeout {
      Some(timeout) => tokio::time::timeout(timeout, handshake).await.ok()?,
      None => handshake.await,
    }
    .ok()?;
    let negotiated = Negotiated::from_alpn(stream.get_ref().1.alpn_protocol());
    Some((stream, negotiated))
  }
}

/// What the client and server agreed on in ALPN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Negotiated {
  /// `http/1.1`, or no ALPN at all (an HTTP/1.1 client).
  Http1,
  /// `h2`.
  Http2,
  /// An extra protocol of [`ServerTlsBuilder::alpn`]: the handshake was its whole purpose.
  Other,
}

impl Negotiated {
  fn from_alpn(alpn: Option<&[u8]>) -> Self {
    match alpn {
      None | Some(ALPN_HTTP11) => Self::Http1,
      Some(ALPN_H2) => Self::Http2,
      Some(_) => Self::Other,
    }
  }
}

/// Builder of a [`ServerTls`] ([`ServerTls::builder`]).
#[must_use]
pub struct ServerTlsBuilder {
  resolver: Arc<dyn rustls::server::ResolvesServerCert>,
  tls12: bool,
  extra_alpn: Vec<Vec<u8>>,
  hooks: Vec<ConfigHook>,
  identity: Option<Identity>,
}

type ConfigHook = Box<dyn FnOnce(&mut rustls::ServerConfig) + Send>;

impl std::fmt::Debug for ServerTlsBuilder {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ServerTlsBuilder")
      .field("tls12", &self.tls12)
      .field("extra_alpn", &self.extra_alpn)
      .field("hooks", &self.hooks.len())
      .field("identity", &self.identity.is_some())
      .finish_non_exhaustive()
  }
}

impl ServerTlsBuilder {
  /// The certificate identity HTTP/3 presents ([`ServerTls::identity`]): QUIC takes an
  /// [`Identity`] rather than the resolver, so without one the settings serve TCP only. Give
  /// the identity the resolver presents for ordinary handshakes (an ACME resolver's issued
  /// certificate, say).
  pub fn identity(mut self, identity: Identity) -> Self {
    self.identity = Some(identity);
    self
  }

  /// Refuse TLS 1.2 clients (TLS 1.3 only).
  pub fn tls13_only(mut self) -> Self {
    self.tls12 = false;
    self
  }

  /// Also offer `protocol` in ALPN, after `h2` and `http/1.1` (for instance `acme-tls/1`, whose
  /// challenge the certificate resolver answers). A connection that negotiates it is closed once
  /// the handshake is done; no HTTP is served on it. `h2` and `http/1.1` are ignored here.
  pub fn alpn(mut self, protocol: impl Into<Vec<u8>>) -> Self {
    let protocol = protocol.into();
    if protocol != ALPN_H2 && protocol != ALPN_HTTP11 && !self.extra_alpn.contains(&protocol) {
      self.extra_alpn.push(protocol);
    }
    self
  }

  /// Change the rustls config once it is built (versions, resolver and ALPN set), in call order.
  /// The hook for ACME TLS-ALPN-01 (feature `acme` of `id_effect_platform`), which installs its
  /// resolver and appends `acme-tls/1`:
  ///
  /// ```rust,ignore
  /// let tls = ServerTls::builder(Arc::new(acme.resolver()))
  ///   .configure(move |config| acme.serve_tls_alpn01(config))
  ///   .build()?;
  /// ```
  ///
  /// Early data stays off whatever a hook sets.
  pub fn configure(
    mut self,
    hook: impl FnOnce(&mut rustls::ServerConfig) + Send + 'static,
  ) -> Self {
    self.hooks.push(Box::new(hook));
    self
  }

  /// The settings.
  ///
  /// # Errors
  ///
  /// [`CertError::Tls`] when rustls refuses the protocol versions for the crypto provider.
  pub fn build(self) -> Result<ServerTls, CertError> {
    let versions: &[&'static rustls::SupportedProtocolVersion] = if self.tls12 {
      &[&rustls::version::TLS13, &rustls::version::TLS12]
    } else {
      &[&rustls::version::TLS13]
    };
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
      .with_protocol_versions(versions)
      .map_err(|e| CertError::Tls(e.to_string()))?
      .with_no_client_auth()
      .with_cert_resolver(self.resolver);
    config.alpn_protocols = [ALPN_H2.to_vec(), ALPN_HTTP11.to_vec()]
      .into_iter()
      .chain(self.extra_alpn)
      .collect();
    for hook in self.hooks {
      hook(&mut config);
    }
    // No 0-RTT on TCP: early data is replayable.
    let mut tls = ServerTls::from_rustls_config(config);
    tls.identity = self.identity;
    Ok(tls)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_platform::tls::NodeCertificate;

  fn identity() -> Identity {
    Identity::from_certificate(&NodeCertificate::self_signed("localhost").unwrap()).unwrap()
  }

  #[test]
  fn defaults_offer_h2_then_http11_on_tls13_and_tls12() {
    let tls = ServerTls::from_identity(&identity()).unwrap();
    let config = tls.rustls_config();
    assert_eq!(
      config.alpn_protocols,
      vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    );
    assert_eq!(config.max_early_data_size, 0);
    assert_eq!(tls.clone(), tls);
    assert_ne!(tls, ServerTls::from_identity(&identity()).unwrap());
    let debug = format!("{tls:?}");
    assert!(debug.contains("reloading: false"), "{debug}");
  }

  /// F24: settings built from an identity carry it for HTTP/3; a bare resolver does not
  /// unless it is given one.
  #[test]
  fn identity_is_carried_for_http3() {
    let identity = identity();
    let tls = ServerTls::from_identity(&identity).unwrap();
    assert_eq!(
      tls.identity().map(Identity::cert_sha256),
      Some(identity.cert_sha256())
    );
    assert!(format!("{tls:?}").contains("identity: true"));
    let bare = ServerTls::builder(Arc::new(identity.clone()));
    assert!(format!("{bare:?}").contains("identity: false"));
    assert!(bare.build().unwrap().identity().is_none());
    let declared = ServerTls::builder(Arc::new(identity.clone()))
      .identity(identity.clone())
      .build()
      .unwrap();
    assert!(declared.identity().is_some());
    let platform =
      id_effect_platform::tls::server_tls_config_with(Arc::new(identity), None).unwrap();
    assert!(ServerTls::from_rustls_config(platform).identity().is_none());
  }

  #[test]
  fn builder_restricts_versions_and_adds_alpn_once() {
    let builder = ServerTls::builder(Arc::new(identity()))
      .tls13_only()
      .alpn("acme-tls/1")
      .alpn("acme-tls/1")
      .alpn("h2")
      .alpn("http/1.1");
    assert!(format!("{builder:?}").contains("tls12: false"));
    let tls = builder.build().unwrap();
    let config = tls.rustls_config();
    assert_eq!(
      config.alpn_protocols,
      vec![b"h2".to_vec(), b"http/1.1".to_vec(), b"acme-tls/1".to_vec()]
    );
  }

  #[test]
  fn configure_hooks_run_in_order_and_early_data_stays_off() {
    let builder = ServerTls::builder(Arc::new(identity()))
      .configure(|config| config.alpn_protocols.push(b"acme-tls/1".to_vec()))
      .configure(|config| {
        assert_eq!(config.alpn_protocols.len(), 3);
        config.max_early_data_size = 16_384;
      });
    assert!(format!("{builder:?}").contains("hooks: 2"));
    let tls = builder.build().unwrap();
    assert_eq!(
      tls.rustls_config().alpn_protocols.last().map(Vec::as_slice),
      Some(&b"acme-tls/1"[..])
    );
    assert_eq!(tls.rustls_config().max_early_data_size, 0);

    let mut platform =
      id_effect_platform::tls::server_tls_config_with(Arc::new(identity()), None).unwrap();
    platform.alpn_protocols = vec![ALPN_H2.to_vec()];
    platform.max_early_data_size = 1;
    let tls = ServerTls::from_rustls_config(platform);
    assert_eq!(tls.rustls_config().max_early_data_size, 0);
    assert_eq!(tls.rustls_config().alpn_protocols, vec![b"h2".to_vec()]);
  }

  #[test]
  fn negotiated_protocol_from_alpn() {
    assert_eq!(Negotiated::from_alpn(None), Negotiated::Http1);
    assert_eq!(Negotiated::from_alpn(Some(b"http/1.1")), Negotiated::Http1);
    assert_eq!(Negotiated::from_alpn(Some(b"h2")), Negotiated::Http2);
    assert_eq!(
      Negotiated::from_alpn(Some(b"acme-tls/1")),
      Negotiated::Other
    );
  }

  #[test]
  fn pem_files_fail_when_missing_or_polling_without_a_runtime() {
    let missing = ServerTls::from_pem_files("/nonexistent/chain.pem", "/nonexistent/key.pem", None);
    assert!(
      matches!(missing, Err(CertError::Read { .. })),
      "{missing:?}"
    );
    let dir = std::env::temp_dir().join(format!("id_effect_axum_tls_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let (chain, key) = (dir.join("chain.pem"), dir.join("key.pem"));
    std::fs::write(&chain, certified.cert.pem()).unwrap();
    std::fs::write(&key, certified.key_pair.serialize_pem()).unwrap();
    let no_runtime = ServerTls::from_pem_files(&chain, &key, Some(Duration::from_secs(1)));
    assert!(
      matches!(&no_runtime, Err(CertError::Tls(m)) if m.contains("Tokio")),
      "{no_runtime:?}"
    );
    let fixed = ServerTls::from_pem_files(&chain, &key, None).unwrap();
    assert!(fixed.reload.is_none());
    assert!(
      fixed.identity().is_some(),
      "certificate files carry their identity"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .unwrap();
    let polling = runtime.block_on(async {
      ServerTls::from_pem_files(&chain, &key, Some(Duration::from_secs(60))).unwrap()
    });
    assert!(format!("{polling:?}").contains("reloading: true"));
    drop(polling);
    drop(runtime);
    let _ = std::fs::remove_dir_all(dir);
  }
}
