//! Certificate material for the encrypted transports (features `quic`, `tls-tcp`).
//!
//! - [`NodeCertificate`] — a node's chain and private key: operator-issued PEM files
//!   ([`NodeCertificate::from_pem_files`]; PKCS#8, PKCS#1 and SEC1 keys) or a throwaway
//!   self-signed certificate ([`NodeCertificate::self_signed`]).
//! - [`load_ca_bundle`] / [`ca_bundle_from_pem`] — the CA roots peers are verified against.
//! - [`CertReloader`] — re-reads the chain and key files when they change, so a rotated
//!   certificate is used for new connections without a restart. It polls (no filesystem
//!   watcher): [`CertReloader::reload_if_changed`] by hand, or [`CertReloader::spawn_polling`].
//!   Connections already open keep the certificate they were established with.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};

/// Certificate material that could not be read or used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CertError {
  /// A file could not be read.
  #[error("certs: cannot read {path}: {message}")]
  Read {
    /// The file.
    path: PathBuf,
    /// The I/O error.
    message: String,
  },
  /// The PEM input is malformed.
  #[error("certs: bad PEM: {0}")]
  Pem(String),
  /// The chain (or CA bundle) holds no `CERTIFICATE` block.
  #[error("certs: no certificate found")]
  NoCertificate,
  /// The key input holds no `PRIVATE KEY`, `RSA PRIVATE KEY` or `EC PRIVATE KEY` block.
  #[error("certs: no private key found")]
  NoPrivateKey,
  /// The key is unsupported or does not belong to the leaf certificate.
  #[error("certs: unusable key: {0}")]
  Key(String),
  /// A certificate could not be generated or added to a root store.
  #[error("certs: {0}")]
  Certificate(String),
}

/// The process default crypto provider if one is installed, else aws-lc-rs (the provider the
/// rest of the workspace compiles in). Every TLS config is built with an explicit provider.
pub(crate) fn provider() -> Arc<rustls::crypto::CryptoProvider> {
  rustls::crypto::CryptoProvider::get_default()
    .cloned()
    .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

/// SHA-256 of a certificate's DER encoding.
pub(crate) fn sha256(cert: &CertificateDer<'_>) -> [u8; 32] {
  Sha256::digest(cert.as_ref()).into()
}

/// One node's certificate chain (leaf first) and private key.
pub struct NodeCertificate {
  chain: Vec<CertificateDer<'static>>,
  key: PrivateKeyDer<'static>,
}

impl Clone for NodeCertificate {
  fn clone(&self) -> Self {
    Self {
      chain: self.chain.clone(),
      key: self.key.clone_key(),
    }
  }
}

impl std::fmt::Debug for NodeCertificate {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("NodeCertificate")
      .field("chain_len", &self.chain.len())
      .field("leaf_sha256", &self.cert_sha256())
      .finish_non_exhaustive()
  }
}

impl NodeCertificate {
  /// A fresh self-signed certificate for `host`. Peers can only trust it by pinning its
  /// fingerprint ([`NodeCertificate::cert_sha256`]); use operator-issued certificates
  /// ([`NodeCertificate::from_pem_files`]) to verify against a CA.
  pub fn self_signed(host: &str) -> Result<Self, CertError> {
    let certified = rcgen::generate_simple_self_signed(vec![host.to_string()])
      .map_err(|e| CertError::Certificate(format!("rcgen: {e}")))?;
    let key = PrivateKeyDer::Pkcs8(certified.key_pair.serialize_der().into());
    Ok(Self {
      chain: vec![certified.cert.der().clone()],
      key,
    })
  }

  /// A chain and key already in DER form. The chain must hold the leaf first.
  pub fn from_der(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
  ) -> Result<Self, CertError> {
    if chain.is_empty() {
      return Err(CertError::NoCertificate);
    }
    let cert = Self { chain, key };
    cert.certified_key()?;
    Ok(cert)
  }

  /// Parse a PEM chain (`CERTIFICATE` blocks, leaf first) and a PEM private key: PKCS#8
  /// (`PRIVATE KEY`), PKCS#1 (`RSA PRIVATE KEY`) or SEC1 (`EC PRIVATE KEY`). Fails when the key
  /// does not belong to the leaf certificate.
  pub fn from_pem(chain_pem: &[u8], key_pem: &[u8]) -> Result<Self, CertError> {
    let chain = CertificateDer::pem_slice_iter(chain_pem)
      .collect::<Result<Vec<_>, _>>()
      .map_err(|e| CertError::Pem(e.to_string()))?;
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| match e {
      rustls::pki_types::pem::Error::NoItemsFound => CertError::NoPrivateKey,
      other => CertError::Pem(other.to_string()),
    })?;
    Self::from_der(chain, key)
  }

  /// Read a PEM chain file and a PEM key file (see [`NodeCertificate::from_pem`]).
  pub fn from_pem_files(chain: impl AsRef<Path>, key: impl AsRef<Path>) -> Result<Self, CertError> {
    Self::from_pem(&read(chain.as_ref())?, &read(key.as_ref())?)
  }

  /// The chain, leaf first.
  pub fn chain(&self) -> &[CertificateDer<'static>] {
    &self.chain
  }

  /// SHA-256 fingerprint of the leaf certificate's DER — the value peers pin.
  pub fn cert_sha256(&self) -> [u8; 32] {
    self.chain.first().map(sha256).unwrap_or_default()
  }

  /// The chain and a signing key loaded by the crypto provider, checked to match.
  pub(crate) fn certified_key(&self) -> Result<Arc<CertifiedKey>, CertError> {
    CertifiedKey::from_der(self.chain.clone(), self.key.clone_key(), &provider())
      .map(Arc::new)
      .map_err(|e| CertError::Key(e.to_string()))
  }
}

fn read(path: &Path) -> Result<Vec<u8>, CertError> {
  std::fs::read(path).map_err(|e| CertError::Read {
    path: path.to_path_buf(),
    message: e.to_string(),
  })
}

/// CA roots from PEM `CERTIFICATE` blocks (a CA bundle). Fails on an empty bundle.
pub fn ca_bundle_from_pem(pem: &[u8]) -> Result<rustls::RootCertStore, CertError> {
  let mut roots = rustls::RootCertStore::empty();
  for cert in CertificateDer::pem_slice_iter(pem) {
    let cert = cert.map_err(|e| CertError::Pem(e.to_string()))?;
    roots
      .add(cert)
      .map_err(|e| CertError::Certificate(e.to_string()))?;
  }
  if roots.is_empty() {
    return Err(CertError::NoCertificate);
  }
  Ok(roots)
}

/// CA roots from a PEM bundle file (see [`ca_bundle_from_pem`]).
pub fn load_ca_bundle(path: impl AsRef<Path>) -> Result<rustls::RootCertStore, CertError> {
  ca_bundle_from_pem(&read(path.as_ref())?)
}

/// The certificate a transport presents, swappable while connections are being made. Each TLS
/// handshake (dialing or accepting) reads the current one.
#[derive(Clone)]
pub(crate) struct Identity(Arc<RwLock<Arc<CertifiedKey>>>);

impl Identity {
  pub(crate) fn new(key: Arc<CertifiedKey>) -> Self {
    Self(Arc::new(RwLock::new(key)))
  }

  pub(crate) fn current(&self) -> Arc<CertifiedKey> {
    Arc::clone(&self.0.read().unwrap_or_else(PoisonError::into_inner))
  }

  fn replace(&self, key: Arc<CertifiedKey>) {
    *self.0.write().unwrap_or_else(PoisonError::into_inner) = key;
  }

  /// Fingerprint of the current leaf certificate.
  pub(crate) fn cert_sha256(&self) -> [u8; 32] {
    self.current().cert.first().map(sha256).unwrap_or_default()
  }
}

impl std::fmt::Debug for Identity {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("Identity")
      .field(&self.cert_sha256())
      .finish()
  }
}

impl rustls::server::ResolvesServerCert for Identity {
  fn resolve(&self, _hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
    Some(self.current())
  }
}

impl rustls::client::ResolvesClientCert for Identity {
  fn resolve(
    &self,
    _root_hint_subjects: &[&[u8]],
    _sigschemes: &[rustls::SignatureScheme],
  ) -> Option<Arc<CertifiedKey>> {
    Some(self.current())
  }

  fn has_certs(&self) -> bool {
    true
  }
}

/// rustls client config for a dialer (TLS 1.3 only) that presents `identity` — the client
/// certificate of mutual TLS, on every dial — and verifies servers: by `pinned` fingerprints
/// (after the `roots` check when both are set), else by `roots`, else not at all. The last is
/// only reachable behind a transport's `dangerous_accept_any_server_cert`; callers check trust
/// first.
pub(crate) fn client_tls_config(
  identity: &Identity,
  roots: Option<&rustls::RootCertStore>,
  pinned: &[[u8; 32]],
) -> Result<rustls::ClientConfig, super::TransportError> {
  use super::TransportError;
  let provider = provider();
  let builder = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
  let builder = if !pinned.is_empty() {
    let roots = match roots {
      Some(roots) => Some(
        rustls::client::WebPkiServerVerifier::builder_with_provider(
          Arc::new(roots.clone()),
          Arc::clone(&provider),
        )
        .build()
        .map_err(|e| TransportError::Io(format!("tls: {e}")))?,
      ),
      None => None,
    };
    builder
      .dangerous()
      .with_custom_certificate_verifier(Arc::new(PinnedCert {
        pins: pinned.to_vec(),
        roots,
        signatures: AcceptAnyCert(provider),
      }))
  } else if let Some(roots) = roots {
    builder.with_root_certificates(roots.clone())
  } else {
    builder
      .dangerous()
      .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
  };
  Ok(builder.with_client_cert_resolver(Arc::new(identity.clone())))
}

/// rustls server config for a listener (TLS 1.3 only) that presents `identity` and requires a
/// client certificate: one with a `client_pinned` fingerprint (after the `client_roots` check
/// when both are set), else one issued under `client_roots`. With neither it requests no client
/// certificate — only reachable behind a transport's `dangerous_no_client_auth`; callers check
/// trust first. Client roots that cannot be used are a [`TransportError::Bind`](super::TransportError::Bind).
pub(crate) fn server_tls_config(
  identity: &Identity,
  client_roots: Option<&rustls::RootCertStore>,
  client_pinned: &[[u8; 32]],
) -> Result<rustls::ServerConfig, super::TransportError> {
  use super::TransportError;
  let provider = provider();
  let builder = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
  let client_roots = match client_roots {
    Some(roots) => Some(
      rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots.clone()),
        Arc::clone(&provider),
      )
      .build()
      .map_err(|e| TransportError::Bind(format!("tls: client roots: {e}")))?,
    ),
    None => None,
  };
  let builder = if !client_pinned.is_empty() {
    builder.with_client_cert_verifier(Arc::new(PinnedClientCert {
      pins: client_pinned.to_vec(),
      roots: client_roots,
      signatures: AcceptAnyCert(provider),
    }))
  } else if let Some(verifier) = client_roots {
    builder.with_client_cert_verifier(verifier)
  } else {
    builder.with_no_client_auth()
  };
  // The resolver reads the current certificate on every handshake, so a
  // reloaded certificate reaches new connections on a running listener.
  Ok(builder.with_cert_resolver(Arc::new(identity.clone())))
}

/// Accepts any server certificate; only behind a transport's
/// `dangerous_accept_any_server_cert`. Its signature checks are shared with
/// [`PinnedCert`] and [`PinnedClientCert`].
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

/// Accepts only pinned leaf certificates, after the root check when roots
/// are configured too.
#[derive(Debug)]
struct PinnedCert {
  pins: Vec<[u8; 32]>,
  roots: Option<Arc<rustls::client::WebPkiServerVerifier>>,
  signatures: AcceptAnyCert,
}

impl rustls::client::danger::ServerCertVerifier for PinnedCert {
  fn verify_server_cert(
    &self,
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
    server_name: &rustls::pki_types::ServerName<'_>,
    ocsp: &[u8],
    now: rustls::pki_types::UnixTime,
  ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
    if let Some(roots) = &self.roots {
      roots.verify_server_cert(end_entity, intermediates, server_name, ocsp, now)?;
    }
    if self.pins.contains(&sha256(end_entity)) {
      Ok(rustls::client::danger::ServerCertVerified::assertion())
    } else {
      Err(rustls::Error::InvalidCertificate(
        rustls::CertificateError::ApplicationVerificationFailure,
      ))
    }
  }

  fn verify_tls12_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    self.signatures.verify_tls12_signature(message, cert, dss)
  }

  fn verify_tls13_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    self.signatures.verify_tls13_signature(message, cert, dss)
  }

  fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
    self.signatures.supported_verify_schemes()
  }
}

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
  fn verify_server_cert(
    &self,
    _end_entity: &CertificateDer<'_>,
    _intermediates: &[CertificateDer<'_>],
    _server_name: &rustls::pki_types::ServerName<'_>,
    _ocsp: &[u8],
    _now: rustls::pki_types::UnixTime,
  ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
    Ok(rustls::client::danger::ServerCertVerified::assertion())
  }

  fn verify_tls12_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls12_signature(
      message,
      cert,
      dss,
      &self.0.signature_verification_algorithms,
    )
  }

  fn verify_tls13_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls13_signature(
      message,
      cert,
      dss,
      &self.0.signature_verification_algorithms,
    )
  }

  fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
    self.0.signature_verification_algorithms.supported_schemes()
  }
}

/// Requires a client certificate with a pinned fingerprint, after the root
/// check when client roots are configured too.
#[derive(Debug)]
struct PinnedClientCert {
  pins: Vec<[u8; 32]>,
  roots: Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>,
  signatures: AcceptAnyCert,
}

impl rustls::server::danger::ClientCertVerifier for PinnedClientCert {
  fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
    self
      .roots
      .as_ref()
      .map_or(&[], |roots| roots.root_hint_subjects())
  }

  fn verify_client_cert(
    &self,
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
    now: rustls::pki_types::UnixTime,
  ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
    if let Some(roots) = &self.roots {
      roots.verify_client_cert(end_entity, intermediates, now)?;
    }
    if self.pins.contains(&sha256(end_entity)) {
      Ok(rustls::server::danger::ClientCertVerified::assertion())
    } else {
      Err(rustls::Error::InvalidCertificate(
        rustls::CertificateError::ApplicationVerificationFailure,
      ))
    }
  }

  fn verify_tls12_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    rustls::client::danger::ServerCertVerifier::verify_tls12_signature(
      &self.signatures,
      message,
      cert,
      dss,
    )
  }

  fn verify_tls13_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    rustls::client::danger::ServerCertVerifier::verify_tls13_signature(
      &self.signatures,
      message,
      cert,
      dss,
    )
  }

  fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
    rustls::client::danger::ServerCertVerifier::supported_verify_schemes(&self.signatures)
  }
}

/// Reloads a node's certificate from its PEM files when they change.
///
/// Transports built from the reloader (`QuicConfig::from_reloader`,
/// `TlsTcpConfig::from_reloader`) present the current
/// certificate on every new connection, dialed or accepted. A change is detected by content (a
/// SHA-256 of both files), not by modification time, so a rotation within the same second is
/// seen. A file that does not parse — say, half-written — is reported and the previous
/// certificate stays in use; the next poll tries again.
pub struct CertReloader {
  chain_path: PathBuf,
  key_path: PathBuf,
  identity: Identity,
  /// Digest of the file contents last loaded.
  loaded: Mutex<[u8; 32]>,
}

impl std::fmt::Debug for CertReloader {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CertReloader")
      .field("chain_path", &self.chain_path)
      .field("key_path", &self.key_path)
      .field("identity", &self.identity)
      .finish()
  }
}

impl CertReloader {
  /// Load the chain and key files now (failing like [`NodeCertificate::from_pem_files`]) and
  /// remember them for later reloads.
  pub fn new(chain: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Result<Self, CertError> {
    let (chain_path, key_path) = (chain.into(), key.into());
    let (digest, cert) = Self::load(&chain_path, &key_path)?;
    Ok(Self {
      identity: Identity::new(cert.certified_key()?),
      chain_path,
      key_path,
      loaded: Mutex::new(digest),
    })
  }

  fn load(chain: &Path, key: &Path) -> Result<([u8; 32], NodeCertificate), CertError> {
    let (chain_pem, key_pem) = (read(chain)?, read(key)?);
    let mut digest = Sha256::new();
    digest.update(&chain_pem);
    digest.update(&key_pem);
    let cert = NodeCertificate::from_pem(&chain_pem, &key_pem)?;
    Ok((digest.finalize().into(), cert))
  }

  /// Re-read the files; when their contents changed, use the new certificate for new
  /// connections. `Ok(true)` when it was swapped. On error the current certificate stays.
  pub fn reload_if_changed(&self) -> Result<bool, CertError> {
    let (chain_pem, key_pem) = (read(&self.chain_path)?, read(&self.key_path)?);
    let mut digest = Sha256::new();
    digest.update(&chain_pem);
    digest.update(&key_pem);
    let digest: [u8; 32] = digest.finalize().into();
    let mut loaded = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
    if *loaded == digest {
      return Ok(false);
    }
    let key = NodeCertificate::from_pem(&chain_pem, &key_pem)?.certified_key()?;
    self.identity.replace(key);
    *loaded = digest;
    Ok(true)
  }

  /// Poll the files every `every` on the current Tokio runtime until the returned handle is
  /// dropped. Failed reloads are logged (`tracing` warn) and retried at the next tick.
  pub fn spawn_polling(self: &Arc<Self>, every: Duration) -> ReloadTask {
    let reloader = Arc::clone(self);
    let task = tokio::spawn(async move {
      let mut ticks = tokio::time::interval(every);
      ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
      loop {
        ticks.tick().await;
        match reloader.reload_if_changed() {
          Ok(true) => tracing::info!(
            chain = %reloader.chain_path.display(),
            "mesh certificate reloaded"
          ),
          Ok(false) => {}
          Err(e) => {
            tracing::warn!(error = %e, "mesh certificate reload failed; keeping the current one")
          }
        }
      }
    });
    ReloadTask(task)
  }

  /// SHA-256 fingerprint of the leaf certificate currently in use.
  pub fn cert_sha256(&self) -> [u8; 32] {
    self.identity.cert_sha256()
  }

  pub(crate) fn identity(&self) -> Identity {
    self.identity.clone()
  }
}

/// The polling task of [`CertReloader::spawn_polling`]; stops when dropped.
#[derive(Debug)]
pub struct ReloadTask(tokio::task::JoinHandle<()>);

impl Drop for ReloadTask {
  fn drop(&mut self) {
    self.0.abort();
  }
}
