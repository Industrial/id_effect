//! Certificate material and rustls configs (feature `tls`; moved from
//! `id_effect_node::transport::certs` in 0.12).
//!
//! - [`NodeCertificate`] — a node's chain and private key: operator-issued PEM files
//!   ([`NodeCertificate::from_pem_files`]; PKCS#8, PKCS#1 and SEC1 keys) or a throwaway
//!   self-signed certificate ([`NodeCertificate::self_signed`]).
//! - [`load_ca_bundle`] / [`ca_bundle_from_pem`] — the CA roots peers are verified against.
//! - [`CertReloader`] — re-reads the chain and key files when they change, so a rotated
//!   certificate is used for new connections without a restart. It polls (no filesystem
//!   watcher): [`CertReloader::reload_if_changed`] by hand, or [`CertReloader::spawn_polling`].
//!   Connections already open keep the certificate they were established with.
//! - [`Identity`] — the swappable certificate a listener or dialer presents.
//! - [`client_tls_config`] / [`server_tls_config`] / [`client_cert_verifier`] — TLS 1.3 rustls
//!   configs with mutual authentication.

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
  /// rustls refused to build a config.
  #[error("tls: {0}")]
  Tls(String),
  /// The client CA roots cannot verify client certificates.
  #[error("tls: client roots: {0}")]
  ClientRoots(String),
}

/// The process default crypto provider if one is installed, else aws-lc-rs (the provider the
/// rest of the workspace compiles in). Every TLS config is built with an explicit provider.
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
  rustls::crypto::CryptoProvider::get_default()
    .cloned()
    .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

/// SHA-256 of a certificate's DER encoding — the fingerprint peers pin.
pub fn sha256(cert: &CertificateDer<'_>) -> [u8; 32] {
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
  pub fn certified_key(&self) -> Result<Arc<CertifiedKey>, CertError> {
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
/// Clones share the slot: a [`CertReloader`] swaps it for every holder.
#[derive(Clone)]
pub struct Identity(Arc<RwLock<Arc<CertifiedKey>>>);

impl Identity {
  /// An identity presenting `key` until it is replaced.
  pub fn new(key: Arc<CertifiedKey>) -> Self {
    Self(Arc::new(RwLock::new(key)))
  }

  /// An identity presenting `cert`. Fails when the crypto provider cannot use the key.
  pub fn from_certificate(cert: &NodeCertificate) -> Result<Self, CertError> {
    Ok(Self::new(cert.certified_key()?))
  }

  /// The certificate presented on the next handshake.
  pub fn current(&self) -> Arc<CertifiedKey> {
    Arc::clone(&self.0.read().unwrap_or_else(PoisonError::into_inner))
  }

  pub(super) fn replace(&self, key: Arc<CertifiedKey>) {
    *self.0.write().unwrap_or_else(PoisonError::into_inner) = key;
  }

  /// Fingerprint of the current leaf certificate.
  pub fn cert_sha256(&self) -> [u8; 32] {
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
pub fn client_tls_config(
  identity: &Identity,
  roots: Option<&rustls::RootCertStore>,
  pinned: &[[u8; 32]],
) -> Result<rustls::ClientConfig, CertError> {
  let provider = provider();
  let builder = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| CertError::Tls(e.to_string()))?;
  let builder = if !pinned.is_empty() {
    let roots = match roots {
      Some(roots) => Some(
        rustls::client::WebPkiServerVerifier::builder_with_provider(
          Arc::new(roots.clone()),
          Arc::clone(&provider),
        )
        .build()
        .map_err(|e| CertError::Tls(e.to_string()))?,
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

/// How a listener with client trust asks for client certificates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientAuth {
  /// Handshake-level mutual TLS: a client without a trusted certificate fails the handshake.
  Required,
  /// Request a certificate, with no CA hints, and let a client that sends none complete the
  /// handshake. A certificate that is sent is still verified, and a bad one fails the
  /// handshake. The application refuses unauthenticated connections where it needs a client
  /// (ADR 0014 D7: the shared QUIC endpoint, where `idm/1` needs one and `h3` does not).
  Optional,
}

/// The client-certificate verifier for `client_roots` and `client_pinned` (see
/// [`server_tls_config`]), asking as `auth` says; `None` when neither is set.
pub fn client_cert_verifier(
  client_roots: Option<&rustls::RootCertStore>,
  client_pinned: &[[u8; 32]],
  auth: ClientAuth,
) -> Result<Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>, CertError> {
  let provider = provider();
  let client_roots = match client_roots {
    Some(roots) => Some(
      rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots.clone()),
        Arc::clone(&provider),
      )
      .build()
      .map_err(|e| CertError::ClientRoots(e.to_string()))?,
    ),
    None => None,
  };
  let verifier: Arc<dyn rustls::server::danger::ClientCertVerifier> = if !client_pinned.is_empty() {
    Arc::new(PinnedClientCert {
      pins: client_pinned.to_vec(),
      roots: client_roots,
      signatures: AcceptAnyCert(provider),
    })
  } else if let Some(verifier) = client_roots {
    verifier
  } else {
    return Ok(None);
  };
  Ok(Some(match auth {
    ClientAuth::Required => verifier,
    ClientAuth::Optional => Arc::new(OptionalClientCert(verifier)),
  }))
}

/// rustls server config for a listener (TLS 1.3 only) that presents `identity` and requires a
/// client certificate: one with a `client_pinned` fingerprint (after the `client_roots` check
/// when both are set), else one issued under `client_roots`. With neither it requests no client
/// certificate — only reachable behind a transport's `dangerous_no_client_auth`; callers check
/// trust first. Client roots that cannot be used are a [`CertError::ClientRoots`].
pub fn server_tls_config(
  identity: &Identity,
  client_roots: Option<&rustls::RootCertStore>,
  client_pinned: &[[u8; 32]],
) -> Result<rustls::ServerConfig, CertError> {
  let verifier = client_cert_verifier(client_roots, client_pinned, ClientAuth::Required)?;
  // The resolver reads the current certificate on every handshake, so a
  // reloaded certificate reaches new connections on a running listener.
  server_tls_config_with(Arc::new(identity.clone()), verifier)
}

/// rustls server config (TLS 1.3 only) resolving its certificate with `resolver` and verifying
/// clients with `verifier` (from [`client_cert_verifier`]; `None` requests no certificate).
/// 0-RTT stays off (`max_early_data_size` 0).
pub fn server_tls_config_with(
  resolver: Arc<dyn rustls::server::ResolvesServerCert>,
  verifier: Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>,
) -> Result<rustls::ServerConfig, CertError> {
  let builder = rustls::ServerConfig::builder_with_provider(provider())
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| CertError::Tls(e.to_string()))?;
  let builder = match verifier {
    Some(verifier) => builder.with_client_cert_verifier(verifier),
    None => builder.with_no_client_auth(),
  };
  Ok(builder.with_cert_resolver(resolver))
}

/// [`ClientAuth::Optional`]: asks for a certificate without naming any CA, lets a client
/// send none, and verifies one that is sent with the wrapped verifier.
#[derive(Debug)]
struct OptionalClientCert(Arc<dyn rustls::server::danger::ClientCertVerifier>);

impl rustls::server::danger::ClientCertVerifier for OptionalClientCert {
  fn offer_client_auth(&self) -> bool {
    true
  }

  fn client_auth_mandatory(&self) -> bool {
    false
  }

  fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
    &[]
  }

  fn verify_client_cert(
    &self,
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
    now: rustls::pki_types::UnixTime,
  ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
    self.0.verify_client_cert(end_entity, intermediates, now)
  }

  fn verify_tls12_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    self.0.verify_tls12_signature(message, cert, dss)
  }

  fn verify_tls13_signature(
    &self,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    self.0.verify_tls13_signature(message, cert, dss)
  }

  fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
    self.0.supported_verify_schemes()
  }
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
/// Transports built from the reloader ([`CertReloader::identity`]; in `id_effect_node`,
/// `QuicConfig::from_reloader` and `TlsTcpConfig::from_reloader`) present the current
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
            "certificate reloaded"
          ),
          Ok(false) => {}
          Err(e) => {
            tracing::warn!(error = %e, "certificate reload failed; keeping the current one")
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

  /// The identity transports present: it follows every successful reload.
  pub fn identity(&self) -> Identity {
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

#[cfg(test)]
mod tests {
  use rustls::client::danger::ServerCertVerifier;
  use rustls::internal::msgs::codec::{Codec, Reader};
  use rustls::pki_types::{ServerName, UnixTime};

  use super::*;

  fn cert(host: &str) -> NodeCertificate {
    NodeCertificate::self_signed(host).expect("cert")
  }

  /// A self-signed leaf as a root (webpki rejects it as an end entity, which is all the
  /// signature-path tests need).
  fn roots_of(cert: &NodeCertificate) -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.chain()[0].clone()).expect("root");
    roots
  }

  /// An ECDSA P-256 signature that cannot verify.
  fn bogus_signature() -> rustls::DigitallySignedStruct {
    rustls::DigitallySignedStruct::read(&mut Reader::init(&[0x04, 0x03, 0x00, 0x01, 0x00]))
      .expect("dss")
  }

  #[test]
  fn certificates_clone_and_debug_without_the_key() {
    let original = cert("node");
    let copy = original.clone();
    assert_eq!(copy.cert_sha256(), original.cert_sha256());
    let shown = format!("{copy:?}");
    assert!(
      shown.contains("chain_len: 1") && !shown.contains("key"),
      "{shown}"
    );
    assert_eq!(
      NodeCertificate::from_der(Vec::new(), original.key.clone_key()).err(),
      Some(CertError::NoCertificate)
    );
    // Malformed base64 inside a chain block and inside a key block.
    let block = |label: &str| format!("-----BEGIN {label}-----\n!!\n-----END {label}-----\n");
    assert!(matches!(
      NodeCertificate::from_pem(block("CERTIFICATE").as_bytes(), b""),
      Err(CertError::Pem(_))
    ));
    let key_label = ["PRIVATE", "KEY"].join(" ");
    assert!(matches!(
      NodeCertificate::from_pem(b"", block(&key_label).as_bytes()),
      Err(CertError::Pem(_))
    ));
    assert_eq!(
      CertError::Tls("x".into()).to_string(),
      "tls: x",
      "messages kept from the node transports"
    );
    let identity = Identity::from_certificate(&original).expect("identity");
    assert!(format!("{identity:?}").starts_with("Identity("));
  }

  #[test]
  fn every_verifier_refuses_a_bad_signature_on_both_versions() {
    let (leaf, other) = (cert("node"), cert("node"));
    let der = &leaf.chain()[0];
    let dss = bogus_signature();
    let provider = provider();
    let server_verifiers: Vec<Box<dyn ServerCertVerifier>> = vec![
      Box::new(AcceptAnyCert(Arc::clone(&provider))),
      Box::new(PinnedCert {
        pins: vec![leaf.cert_sha256()],
        roots: None,
        signatures: AcceptAnyCert(Arc::clone(&provider)),
      }),
    ];
    for verifier in &server_verifiers {
      assert!(verifier.verify_tls12_signature(b"m", der, &dss).is_err());
      assert!(verifier.verify_tls13_signature(b"m", der, &dss).is_err());
      assert!(!verifier.supported_verify_schemes().is_empty());
    }
    let name = ServerName::try_from("node").expect("name");
    let pinned = &server_verifiers[1];
    assert!(
      pinned
        .verify_server_cert(der, &[], &name, &[], UnixTime::now())
        .is_ok()
    );
    assert!(
      pinned
        .verify_server_cert(&other.chain()[0], &[], &name, &[], UnixTime::now())
        .is_err()
    );

    for auth in [ClientAuth::Required, ClientAuth::Optional] {
      for (roots, pins) in [
        (Some(roots_of(&leaf)), vec![]),
        (None, vec![leaf.cert_sha256()]),
      ] {
        let verifier = client_cert_verifier(roots.as_ref(), &pins, auth)
          .expect("verifier")
          .expect("trust given");
        assert_eq!(
          verifier.client_auth_mandatory(),
          auth == ClientAuth::Required
        );
        assert!(verifier.offer_client_auth());
        if auth == ClientAuth::Optional {
          assert!(verifier.root_hint_subjects().is_empty(), "no CA hints");
        }
        assert!(verifier.verify_tls12_signature(b"m", der, &dss).is_err());
        assert!(verifier.verify_tls13_signature(b"m", der, &dss).is_err());
        assert!(!verifier.supported_verify_schemes().is_empty());
        assert!(
          verifier
            .verify_client_cert(&other.chain()[0], &[], UnixTime::now())
            .is_err()
        );
      }
    }
    // Pins with roots: both must pass, and the hints are the roots'.
    let both = client_cert_verifier(
      Some(&roots_of(&leaf)),
      &[leaf.cert_sha256()],
      ClientAuth::Required,
    )
    .expect("verifier")
    .expect("trust");
    assert!(!both.root_hint_subjects().is_empty());
    assert!(
      client_cert_verifier(None, &[], ClientAuth::Optional)
        .expect("no trust")
        .is_none()
    );
  }

  #[tokio::test]
  async fn the_reloader_debugs_its_paths_and_keeps_polling_after_a_bad_file() {
    let dir = std::env::temp_dir().join(format!(
      "id-effect-platform-reload-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let certified = rcgen::generate_simple_self_signed(vec!["node".into()]).expect("rcgen");
    let (chain, key) = (dir.join("node.crt"), dir.join("node.key"));
    std::fs::write(&chain, certified.cert.pem()).expect("chain");
    std::fs::write(&key, certified.key_pair.serialize_pem()).expect("key");
    let reloader = Arc::new(CertReloader::new(&chain, &key).expect("load"));
    assert!(format!("{reloader:?}").contains("node.crt"));
    let before = reloader.cert_sha256();
    assert_eq!(reloader.identity().cert_sha256(), before);

    // A broken file is logged and retried; a good one is picked up.
    std::fs::write(&key, "not a key").expect("break");
    let task = reloader.spawn_polling(Duration::from_millis(5));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(reloader.cert_sha256(), before);
    let next = rcgen::generate_simple_self_signed(vec!["node".into()]).expect("rcgen");
    std::fs::write(&chain, next.cert.pem()).expect("chain");
    std::fs::write(&key, next.key_pair.serialize_pem()).expect("key");
    tokio::time::timeout(Duration::from_secs(5), async {
      while reloader.cert_sha256() == before {
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
    })
    .await
    .expect("reloaded");
    drop(task);
    let _ = std::fs::remove_dir_all(&dir);
  }
}
