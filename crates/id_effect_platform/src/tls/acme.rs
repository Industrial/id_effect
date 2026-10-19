//! **ACME certificates over TLS-ALPN-01** (feature `acme`; ADR 0014 D8, RFC 8555, RFC 8737).
//!
//! [`Acme`] orders a certificate for its domains from an ACME directory (Let's Encrypt staging
//! by default), proves control of each domain with the `tls-alpn-01` challenge on the TLS port
//! the application already serves, and installs the issued certificate into its [`Identity`] —
//! the slot the QUIC endpoint and the TCP TLS listener read on every handshake, so new
//! connections use it without a restart and open ones are not cut ([`Identity::install`]). It
//! renews at two thirds of the certificate's lifetime, minus a random jitter of up to a
//! twentieth of it so a fleet does not renew in step. HTTP-01 and DNS-01 are not offered: the
//! first needs port 80, the second a DNS provider integration (ADR 0014 D8).
//!
//! The challenge is answered by [`AcmeResolver`], the rustls certificate resolver of the TCP
//! TLS listener: a handshake that offers only the `acme-tls/1` ALPN for a domain with a pending
//! challenge gets that challenge's certificate, every other handshake the current certificate.
//! [`Acme::serve_tls_alpn01`] puts the resolver and the ALPN on a listener's
//! `rustls::ServerConfig`; the listener closes a connection that negotiated `acme-tls/1`
//! ([`is_acme_challenge`]) right after the handshake. The QUIC endpoint needs nothing: ACME
//! validates over TCP.
//!
//! Failure keeps what works: an order that fails — the directory is unreachable, a challenge is
//! refused, the certificate does not parse — leaves the current certificate in place, logs a
//! `tracing` warning and counts it ([`AcmeStats::failed`]); [`Acme::spawn`] retries after
//! [`AcmeConfig::retry_after_failure`].
//!
//! Key material: the account credentials (`account-<directory>.json`) and the certificate key
//! (`key.pem`) live in the cache directory (mode 0700) with mode 0600, a looser mode found on
//! start is tightened, and in memory they are held as `id_effect_config::Secret` (zeroized on
//! drop, redacted in `Debug`). Neither appears in `Debug` output or in logs.
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use id_effect_platform::tls::acme::{Acme, AcmeConfig};
//! use id_effect_platform::tls::server_tls_config_with;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let acme = Acme::new(
//!   AcmeConfig::new(["app.example.com"], "/var/lib/app/acme")
//!     .production()
//!     .contact("mailto:ops@example.com")
//!     .agree_to_terms_of_service(),
//! )?;
//! // The TCP TLS listener answers the challenge; the QUIC endpoint takes `acme.identity()`.
//! let mut tcp = server_tls_config_with(Arc::new(acme.resolver()), None)?;
//! tcp.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
//! acme.serve_tls_alpn01(&mut tcp);
//! let _renewals = acme.spawn(); // issues now when there is no cached certificate
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use id_effect_config::Secret;
use instant_acme::{
  Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
  NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};

use super::reload::fingerprint_hex;
use super::{CertError, Identity, NodeCertificate};

/// The ALPN protocol of the TLS-ALPN-01 challenge (RFC 8737 §6.2).
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// Let's Encrypt's staging directory — the default, for every profile but production.
pub const LETS_ENCRYPT_STAGING: &str = LetsEncrypt::Staging.url();

/// Let's Encrypt's production directory ([`AcmeConfig::production`]).
pub const LETS_ENCRYPT_PRODUCTION: &str = LetsEncrypt::Production.url();

/// The longest single sleep of [`Acme::spawn`]; it re-reads the clock after each.
const MAX_SLEEP: Duration = Duration::from_secs(3600);

/// An ACME setting, account, order or certificate that failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AcmeError {
  /// The configuration cannot be used.
  #[error("acme: config: {0}")]
  Config(String),
  /// The account could not be created, loaded or stored.
  #[error("acme: account: {0}")]
  Account(String),
  /// The order, an authorization or a challenge failed.
  #[error("acme: order: {0}")]
  Order(String),
  /// The issued certificate or a generated key is unusable.
  #[error("acme: {0}")]
  Certificate(#[from] CertError),
  /// The cache directory could not be read or written.
  #[error("acme: cannot write {path}: {message}")]
  Io {
    /// The file or directory.
    path: PathBuf,
    /// The I/O error.
    message: String,
  },
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> AcmeError + '_ {
  move |e| AcmeError::Io {
    path: path.to_path_buf(),
    message: e.to_string(),
  }
}

fn order_error(e: impl fmt::Display) -> AcmeError {
  AcmeError::Order(e.to_string())
}

fn rcgen_error(e: rcgen::Error) -> AcmeError {
  AcmeError::Certificate(CertError::Certificate(format!("rcgen: {e}")))
}

/// What [`Acme`] orders and where it keeps its state.
#[derive(Clone, Debug)]
pub struct AcmeConfig {
  domains: Vec<String>,
  cache_dir: PathBuf,
  directory_url: String,
  directory_root: Option<PathBuf>,
  contacts: Vec<String>,
  terms_agreed: bool,
  retry_after_failure: Duration,
  order_timeout: Duration,
}

impl AcmeConfig {
  /// Certificates for `domains` (DNS names, no wildcards: TLS-ALPN-01 cannot prove one), with
  /// the account, certificate and key kept in `cache_dir`. The directory is Let's Encrypt
  /// staging until [`AcmeConfig::production`] or [`AcmeConfig::directory_url`].
  pub fn new(
    domains: impl IntoIterator<Item = impl Into<String>>,
    cache_dir: impl Into<PathBuf>,
  ) -> Self {
    Self {
      domains: domains.into_iter().map(Into::into).collect(),
      cache_dir: cache_dir.into(),
      directory_url: LETS_ENCRYPT_STAGING.to_string(),
      directory_root: None,
      contacts: Vec::new(),
      terms_agreed: false,
      retry_after_failure: Duration::from_secs(3600),
      order_timeout: Duration::from_secs(120),
    }
  }

  /// Use Let's Encrypt's production directory (the prod profile; its certificates are
  /// publicly trusted and rate limited).
  pub fn production(self) -> Self {
    self.directory_url(LETS_ENCRYPT_PRODUCTION)
  }

  /// Use the ACME directory at `url` (`https://` only).
  pub fn directory_url(mut self, url: impl Into<String>) -> Self {
    self.directory_url = url.into();
    self
  }

  /// Trust the CA in the PEM file at `path` (and only it) for the connection to the directory,
  /// for a directory on a private PKI such as Pebble's. Without it the platform's trust store
  /// verifies the directory.
  pub fn directory_root(mut self, path: impl Into<PathBuf>) -> Self {
    self.directory_root = Some(path.into());
    self
  }

  /// Add a contact URI for the account (`mailto:ops@example.com`).
  pub fn contact(mut self, uri: impl Into<String>) -> Self {
    self.contacts.push(uri.into());
    self
  }

  /// Agree to the directory's terms of service — required to create an account.
  pub fn agree_to_terms_of_service(mut self) -> Self {
    self.terms_agreed = true;
    self
  }

  /// How long [`Acme::spawn`] waits after a failed order before it tries again (default one
  /// hour).
  pub fn retry_after_failure(mut self, wait: Duration) -> Self {
    self.retry_after_failure = wait;
    self
  }

  /// How long one order may wait for the directory to validate and issue (default two
  /// minutes).
  pub fn order_timeout(mut self, timeout: Duration) -> Self {
    self.order_timeout = timeout;
    self
  }

  /// The domains, lower-cased.
  pub fn domains(&self) -> &[String] {
    &self.domains
  }

  /// The directory URL.
  pub fn directory(&self) -> &str {
    &self.directory_url
  }

  fn validate(mut self) -> Result<Self, AcmeError> {
    if self.domains.is_empty() {
      return Err(AcmeError::Config("no domains".into()));
    }
    for domain in &mut self.domains {
      domain.make_ascii_lowercase();
      let is_dns = matches!(
        rustls::pki_types::ServerName::try_from(domain.as_str()),
        Ok(rustls::pki_types::ServerName::DnsName(_))
      );
      if domain.contains('*') || !is_dns {
        return Err(AcmeError::Config(format!(
          "{domain:?} is not a DNS name TLS-ALPN-01 can prove (no wildcards, no IP addresses)"
        )));
      }
    }
    self.domains.sort();
    self.domains.dedup();
    if !self
      .directory_url
      .get(..8)
      .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
      return Err(AcmeError::Config(format!(
        "directory {:?} is not https",
        self.directory_url
      )));
    }
    if !self.terms_agreed {
      return Err(AcmeError::Config(
        "the directory's terms of service are not agreed (AcmeConfig::agree_to_terms_of_service)"
          .into(),
      ));
    }
    Ok(self)
  }

  /// The account file for this directory: accounts are per directory.
  fn account_path(&self) -> PathBuf {
    let digest = Sha256::digest(self.directory_url.as_bytes());
    let mut short = [0u8; 32];
    short.copy_from_slice(&digest);
    let name = format!("account-{}.json", &fingerprint_hex(&short)[..16]);
    self.cache_dir.join(name)
  }
}

/// Counters and times of an [`Acme`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AcmeStats {
  /// Certificates issued and installed.
  pub issued: u64,
  /// Orders that failed (the current certificate stayed).
  pub failed: u64,
  /// When the installed certificate expires; `None` while the placeholder is presented.
  pub not_after: Option<SystemTime>,
  /// When it is due for renewal; `None` while the placeholder is presented (due now).
  pub renew_at: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Validity {
  not_before: SystemTime,
  not_after: SystemTime,
}

/// Two thirds into the lifetime, minus up to a twentieth of it chosen by `seed`.
fn renewal_time(validity: Validity, seed: u64) -> SystemTime {
  let lifetime = validity
    .not_after
    .duration_since(validity.not_before)
    .unwrap_or_default();
  let window = (lifetime / 20).as_nanos();
  let jitter = if window == 0 {
    Duration::ZERO
  } else {
    Duration::from_nanos((u128::from(seed) % window) as u64)
  };
  validity.not_before + lifetime * 2 / 3 - jitter
}

/// A random per-process seed for the jitter (std's randomly keyed hasher; no RNG dependency).
fn jitter_seed() -> u64 {
  let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
  hasher.write_u64(0x6163_6d65);
  hasher.finish()
}

/// The validity and the DNS names of a certificate.
fn inspect(cert: &CertificateDer<'_>) -> Result<(Validity, Vec<String>), CertError> {
  let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
    .map_err(|e| CertError::Certificate(format!("x509: {e}")))?;
  let at = |t: x509_parser::time::ASN1Time| {
    let secs = t.timestamp();
    if secs >= 0 {
      SystemTime::UNIX_EPOCH + Duration::from_secs(secs.unsigned_abs())
    } else {
      SystemTime::UNIX_EPOCH - Duration::from_secs(secs.unsigned_abs())
    }
  };
  let validity = Validity {
    not_before: at(parsed.validity().not_before),
    not_after: at(parsed.validity().not_after),
  };
  let names = match parsed.subject_alternative_name() {
    Ok(Some(san)) => san
      .value
      .general_names
      .iter()
      .filter_map(|name| match name {
        x509_parser::extensions::GeneralName::DNSName(dns) => Some(dns.to_ascii_lowercase()),
        _ => None,
      })
      .collect(),
    _ => Vec::new(),
  };
  Ok((validity, names))
}

/// Pending TLS-ALPN-01 answers, by lower-cased domain.
type Challenges = Arc<RwLock<HashMap<String, Arc<CertifiedKey>>>>;

/// The certificate resolver of a TCP TLS listener under ACME: a handshake offering exactly the
/// `acme-tls/1` ALPN gets the pending challenge certificate for its SNI name (none pending:
/// no certificate, the handshake fails); every other handshake gets the [`Identity`]'s current
/// certificate. Clones share the state.
#[derive(Clone)]
pub struct AcmeResolver {
  identity: Identity,
  challenges: Challenges,
}

impl fmt::Debug for AcmeResolver {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let pending = self
      .challenges
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .len();
    f.debug_struct("AcmeResolver")
      .field("identity", &self.identity)
      .field("pending_challenges", &pending)
      .finish()
  }
}

impl rustls::server::ResolvesServerCert for AcmeResolver {
  fn resolve(&self, hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
    let challenge = hello
      .alpn()
      .is_some_and(|mut offered| offered.next() == Some(ACME_TLS_ALPN) && offered.next().is_none());
    if !challenge {
      return Some(self.identity.current());
    }
    let name = hello.server_name()?.to_ascii_lowercase();
    self
      .challenges
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .get(&name)
      .cloned()
  }
}

/// Whether a connection negotiated the challenge ALPN: the listener closes it after the
/// handshake and serves it nothing.
pub fn is_acme_challenge(negotiated_alpn: Option<&[u8]>) -> bool {
  negotiated_alpn == Some(ACME_TLS_ALPN)
}

/// The self-signed TLS-ALPN-01 answer for `domain`: its only SAN is `domain`, and the critical
/// `acmeIdentifier` extension holds the SHA-256 of the key authorization (RFC 8737 §3).
fn challenge_certificate(domain: &str, digest: &[u8]) -> Result<Arc<CertifiedKey>, AcmeError> {
  let mut params = rcgen::CertificateParams::new(vec![domain.to_string()]).map_err(rcgen_error)?;
  params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
  let key = rcgen::KeyPair::generate().map_err(rcgen_error)?;
  let cert = params.self_signed(&key).map_err(rcgen_error)?;
  let key = Secret::new(key.serialize_der());
  // Not through `NodeCertificate`: its key check parses the certificate with webpki, which
  // refuses the critical `acmeIdentifier` extension it does not know.
  let signer = super::provider()
    .key_provider
    .load_private_key(PrivateKeyDer::Pkcs8(key.expose().clone().into()))
    .map_err(|e| CertError::Key(e.to_string()))?;
  Ok(Arc::new(CertifiedKey::new(
    vec![cert.der().clone()],
    signer,
  )))
}

/// A fresh ECDSA P-256 key (PEM) and a CSR for `domains`.
fn certificate_request(domains: &[String]) -> Result<(Secret<String>, Vec<u8>), AcmeError> {
  let mut params = rcgen::CertificateParams::new(domains.to_vec()).map_err(rcgen_error)?;
  params.distinguished_name = rcgen::DistinguishedName::new();
  let key = rcgen::KeyPair::generate().map_err(rcgen_error)?;
  let csr = params.serialize_request(&key).map_err(rcgen_error)?;
  Ok((Secret::new(key.serialize_pem()), csr.der().to_vec()))
}

/// Removes the challenge answers of one order when it ends, however it ends.
struct ChallengeGuard {
  challenges: Challenges,
  names: Vec<String>,
}

impl ChallengeGuard {
  fn insert(&mut self, name: String, answer: Arc<CertifiedKey>) {
    self
      .challenges
      .write()
      .unwrap_or_else(PoisonError::into_inner)
      .insert(name.clone(), answer);
    self.names.push(name);
  }
}

impl Drop for ChallengeGuard {
  fn drop(&mut self) {
    let mut challenges = self
      .challenges
      .write()
      .unwrap_or_else(PoisonError::into_inner);
    for name in &self.names {
      challenges.remove(name);
    }
  }
}

/// Create `dir` (mode 0700 on Unix).
fn ensure_private_dir(dir: &Path) -> Result<(), AcmeError> {
  std::fs::create_dir_all(dir).map_err(io_error(dir))?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(io_error(dir))?;
  }
  Ok(())
}

/// Write `bytes` to `path` with mode 0600, through a temporary file renamed into place.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), AcmeError> {
  let tmp = path.with_extension("tmp");
  let _ = std::fs::remove_file(&tmp);
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
  let mut file = options.open(&tmp).map_err(io_error(&tmp))?;
  file.write_all(bytes).map_err(io_error(&tmp))?;
  file.sync_all().map_err(io_error(&tmp))?;
  drop(file);
  std::fs::rename(&tmp, path).map_err(io_error(path))?;
  tighten(path)
}

/// Make a key file readable by its owner only (mode 0600); a looser mode is logged.
fn tighten(path: &Path) -> Result<(), AcmeError> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
      .map_err(io_error(path))?
      .permissions()
      .mode();
    if mode & 0o077 != 0 {
      tracing::warn!(path = %path.display(), mode = format!("{:o}", mode & 0o777), "key file readable by others; setting mode 0600");
      std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(io_error(path))?;
    }
  }
  #[cfg(not(unix))]
  let _ = path;
  Ok(())
}

/// Read a key file into a [`Secret`] after tightening its mode; `None` when it does not exist.
fn read_private(path: &Path) -> Result<Option<Secret<Vec<u8>>>, AcmeError> {
  if !path.exists() {
    return Ok(None);
  }
  tighten(path)?;
  std::fs::read(path)
    .map(|bytes| Some(Secret::new(bytes)))
    .map_err(io_error(path))
}

struct Inner {
  config: AcmeConfig,
  identity: Identity,
  challenges: Challenges,
  account: tokio::sync::Mutex<Option<Account>>,
  validity: Mutex<Option<Validity>>,
  seed: u64,
  issued: AtomicU64,
  failed: AtomicU64,
}

/// Issues and renews the certificate of an [`Identity`] over ACME TLS-ALPN-01 (see the module
/// documentation). Clones share the state.
#[derive(Clone)]
pub struct Acme {
  inner: Arc<Inner>,
}

impl fmt::Debug for Acme {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Acme")
      .field("domains", &self.inner.config.domains)
      .field("directory", &self.inner.config.directory_url)
      .field("identity", &self.inner.identity)
      .field("stats", &self.stats())
      .finish_non_exhaustive()
  }
}

impl Acme {
  /// Validate `config`, create the cache directory, and start presenting the cached
  /// certificate when it is still valid and covers the domains — else a self-signed placeholder
  /// for the first domain until the first order ([`Acme::issue`], [`Acme::spawn`]) succeeds.
  pub fn new(config: AcmeConfig) -> Result<Self, AcmeError> {
    let config = config.validate()?;
    ensure_private_dir(&config.cache_dir)?;
    let cached = Self::cached(&config).unwrap_or_else(|e| {
      tracing::warn!(error = %e, "cached ACME certificate unusable; ordering a new one");
      None
    });
    let (cert, validity) = match cached {
      Some((cert, validity)) => (cert, Some(validity)),
      None => (NodeCertificate::self_signed(&config.domains[0])?, None),
    };
    Ok(Self {
      inner: Arc::new(Inner {
        identity: Identity::from_certificate(&cert)?,
        config,
        challenges: Arc::default(),
        account: tokio::sync::Mutex::new(None),
        validity: Mutex::new(validity),
        seed: jitter_seed(),
        issued: AtomicU64::new(0),
        failed: AtomicU64::new(0),
      }),
    })
  }

  /// The cached certificate when it is unexpired and names every domain.
  fn cached(config: &AcmeConfig) -> Result<Option<(NodeCertificate, Validity)>, AcmeError> {
    let (chain_path, key_path) = (
      config.cache_dir.join("cert.pem"),
      config.cache_dir.join("key.pem"),
    );
    let Some(key) = read_private(&key_path)? else {
      return Ok(None);
    };
    let chain = std::fs::read(&chain_path).map_err(io_error(&chain_path))?;
    let cert = NodeCertificate::from_pem(&chain, key.expose())?;
    let (validity, names) = inspect(&cert.chain()[0])?;
    let covers = config.domains.iter().all(|d| names.contains(d));
    Ok((covers && validity.not_after > SystemTime::now()).then_some((cert, validity)))
  }

  /// The identity carrying the current certificate: give clones to the QUIC endpoint and the
  /// TCP listener. Every issued certificate is installed into it.
  pub fn identity(&self) -> Identity {
    self.inner.identity.clone()
  }

  /// The certificate resolver for the TCP TLS listener (answers the challenge).
  pub fn resolver(&self) -> AcmeResolver {
    AcmeResolver {
      identity: self.identity(),
      challenges: Arc::clone(&self.inner.challenges),
    }
  }

  /// Make a TCP TLS listener's config answer TLS-ALPN-01: its certificate resolver becomes
  /// [`Acme::resolver`] and `acme-tls/1` joins its ALPN list (last, so clients that offer
  /// `h2` or `http/1.1` never select it). The listener must not require client certificates.
  pub fn serve_tls_alpn01(&self, config: &mut rustls::ServerConfig) {
    config.cert_resolver = Arc::new(self.resolver());
    if !config.alpn_protocols.iter().any(|p| p == ACME_TLS_ALPN) {
      config.alpn_protocols.push(ACME_TLS_ALPN.to_vec());
    }
  }

  /// Counters and times.
  pub fn stats(&self) -> AcmeStats {
    let validity = *self
      .inner
      .validity
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    AcmeStats {
      issued: self.inner.issued.load(Ordering::Relaxed),
      failed: self.inner.failed.load(Ordering::Relaxed),
      not_after: validity.map(|v| v.not_after),
      renew_at: validity.map(|v| renewal_time(v, self.inner.seed)),
    }
  }

  /// How long until the certificate is due for renewal (zero when due, or for the placeholder).
  fn due_in(&self) -> Duration {
    self
      .stats()
      .renew_at
      .and_then(|at| at.duration_since(SystemTime::now()).ok())
      .unwrap_or_default()
  }

  /// Run one order now and install the certificate it yields; returns its fingerprint. On
  /// failure the current certificate stays, the failure is logged and counted.
  pub async fn issue(&self) -> Result<[u8; 32], AcmeError> {
    match self.order().await {
      Ok((fingerprint, validity)) => {
        self.inner.issued.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
          domains = ?self.inner.config.domains,
          sha256 = %fingerprint_hex(&fingerprint),
          not_after = ?validity.not_after,
          "ACME certificate issued"
        );
        Ok(fingerprint)
      }
      Err(e) => {
        self.inner.failed.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
          domains = ?self.inner.config.domains,
          error = %e,
          "ACME order failed; keeping the current certificate"
        );
        Err(e)
      }
    }
  }

  /// Renew in the background on the current Tokio runtime until the handle is dropped: at once
  /// when the placeholder is presented, else when [`AcmeStats::renew_at`] comes; after a
  /// failure, again after [`AcmeConfig::retry_after_failure`].
  pub fn spawn(&self) -> AcmeTask {
    let acme = self.clone();
    AcmeTask(tokio::spawn(async move {
      loop {
        let due = acme.due_in();
        if !due.is_zero() {
          tokio::time::sleep(due.min(MAX_SLEEP)).await;
          continue;
        }
        if acme.issue().await.is_err() {
          tokio::time::sleep(acme.inner.config.retry_after_failure).await;
        }
      }
    }))
  }

  async fn account(&self) -> Result<Account, AcmeError> {
    let mut slot = self.inner.account.lock().await;
    if let Some(account) = slot.as_ref() {
      return Ok(account.clone());
    }
    let config = &self.inner.config;
    let account_error = |e: instant_acme::Error| AcmeError::Account(e.to_string());
    let builder = || match &config.directory_root {
      Some(root) => Account::builder_with_root(root).map_err(account_error),
      None => Account::builder().map_err(account_error),
    };
    let path = config.account_path();
    let account = match read_private(&path)? {
      Some(stored) => {
        let credentials: AccountCredentials = serde_json::from_slice(stored.expose())
          .map_err(|e| AcmeError::Account(format!("{}: {e}", path.display())))?;
        builder()?
          .from_credentials(credentials)
          .await
          .map_err(account_error)?
      }
      None => {
        let contacts: Vec<&str> = config.contacts.iter().map(String::as_str).collect();
        let (account, credentials) = builder()?
          .create(
            &NewAccount {
              contact: &contacts,
              terms_of_service_agreed: true,
              only_return_existing: false,
            },
            config.directory_url.clone(),
            None,
          )
          .await
          .map_err(account_error)?;
        let stored = Secret::new(
          serde_json::to_vec(&credentials).map_err(|e| AcmeError::Account(e.to_string()))?,
        );
        write_private(&path, stored.expose())?;
        tracing::info!(directory = %config.directory_url, "ACME account created");
        account
      }
    };
    *slot = Some(account.clone());
    Ok(account)
  }

  async fn order(&self) -> Result<([u8; 32], Validity), AcmeError> {
    let config = &self.inner.config;
    let account = self.account().await?;
    let identifiers: Vec<Identifier> = config
      .domains
      .iter()
      .map(|d| Identifier::Dns(d.clone()))
      .collect();
    let mut order = account
      .new_order(&NewOrder::new(&identifiers))
      .await
      .map_err(order_error)?;
    let mut answers = ChallengeGuard {
      challenges: Arc::clone(&self.inner.challenges),
      names: Vec::new(),
    };
    let mut authorizations = order.authorizations();
    while let Some(authorization) = authorizations.next().await {
      let mut authorization = authorization.map_err(order_error)?;
      match authorization.status {
        AuthorizationStatus::Pending => {}
        AuthorizationStatus::Valid => continue,
        other => return Err(AcmeError::Order(format!("authorization is {other:?}"))),
      }
      let mut challenge = authorization
        .challenge(ChallengeType::TlsAlpn01)
        .ok_or_else(|| AcmeError::Order("the directory offers no tls-alpn-01 challenge".into()))?;
      let domain = match challenge.identifier().identifier {
        Identifier::Dns(domain) => domain.to_ascii_lowercase(),
        other => return Err(AcmeError::Order(format!("unexpected identifier {other:?}"))),
      };
      let answer = challenge_certificate(&domain, challenge.key_authorization().digest().as_ref())?;
      answers.insert(domain, answer);
      challenge.set_ready().await.map_err(order_error)?;
    }
    let retries = RetryPolicy::new().timeout(config.order_timeout);
    let status = order.poll_ready(&retries).await.map_err(order_error)?;
    if status != OrderStatus::Ready {
      return Err(AcmeError::Order(format!("order is {status:?}")));
    }
    let (key_pem, csr) = certificate_request(&config.domains)?;
    order.finalize_csr(&csr).await.map_err(order_error)?;
    let chain_pem = order
      .poll_certificate(&retries)
      .await
      .map_err(order_error)?;
    drop(answers);
    self.install(chain_pem.as_bytes(), &key_pem)
  }

  /// Check, store and install an issued chain and its key.
  fn install(
    &self,
    chain_pem: &[u8],
    key_pem: &Secret<String>,
  ) -> Result<([u8; 32], Validity), AcmeError> {
    let cert = NodeCertificate::from_pem(chain_pem, key_pem.expose().as_bytes())?;
    let (validity, names) = inspect(&cert.chain()[0])?;
    if let Some(missing) = self
      .inner
      .config
      .domains
      .iter()
      .find(|d| !names.contains(d))
    {
      return Err(AcmeError::Order(format!(
        "the issued certificate does not name {missing}"
      )));
    }
    let dir = &self.inner.config.cache_dir;
    write_private(&dir.join("key.pem"), key_pem.expose().as_bytes())?;
    write_private(&dir.join("cert.pem"), chain_pem)?;
    let fingerprint = self.inner.identity.install(&cert)?;
    *self
      .inner
      .validity
      .lock()
      .unwrap_or_else(PoisonError::into_inner) = Some(validity);
    Ok((fingerprint, validity))
  }
}

/// The renewal task of [`Acme::spawn`]; stops when dropped.
#[derive(Debug)]
pub struct AcmeTask(tokio::task::JoinHandle<()>);

impl Drop for AcmeTask {
  fn drop(&mut self) {
    self.0.abort();
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use rustls::pki_types::pem::PemObject;

  use super::*;
  use crate::tls::server_tls_config_with;

  const DOMAIN: &str = "app.example.test";

  /// A directory URL on a loopback port nothing listens on.
  fn closed_directory() -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
      .expect("bind")
      .local_addr()
      .expect("addr")
      .port();
    format!("https://127.0.0.1:{port}/dir")
  }

  fn config(dir: &Path) -> AcmeConfig {
    AcmeConfig::new([DOMAIN], dir)
      .directory_url(closed_directory())
      .agree_to_terms_of_service()
  }

  /// A self-signed certificate (chain PEM, key PEM) for `name`, valid until `not_after`.
  fn pem_for(name: &str, not_after: Option<(i32, u8, u8)>) -> (String, Secret<String>) {
    let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).expect("params");
    if let Some((y, m, d)) = not_after {
      params.not_before = rcgen::date_time_ymd(y - 1, m, d);
      params.not_after = rcgen::date_time_ymd(y, m, d);
    }
    let key = rcgen::KeyPair::generate().expect("key");
    let cert = params.self_signed(&key).expect("cert");
    (cert.pem(), Secret::new(key.serialize_pem()))
  }

  fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("meta").permissions().mode() & 0o777
  }

  /// A TCP-listener config answering TLS-ALPN-01 for `acme`.
  fn listener(acme: &Acme) -> rustls::ServerConfig {
    let mut config = server_tls_config_with(Arc::new(acme.resolver()), None).expect("config");
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    acme.serve_tls_alpn01(&mut config);
    config
  }

  /// Accepts every certificate and signature: webpki refuses the critical `acmeIdentifier`
  /// extension even for a signature check, and ACME validators read the certificate instead.
  #[derive(Debug)]
  struct Validator;

  impl rustls::client::danger::ServerCertVerifier for Validator {
    fn verify_server_cert(
      &self,
      _: &CertificateDer<'_>,
      _: &[CertificateDer<'_>],
      _: &rustls::pki_types::ServerName<'_>,
      _: &[u8],
      _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
      Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
      &self,
      _: &[u8],
      _: &CertificateDer<'_>,
      _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
      Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
      &self,
      _: &[u8],
      _: &CertificateDer<'_>,
      _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
      Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
      crate::tls::provider()
        .signature_verification_algorithms
        .supported_schemes()
    }
  }

  /// An in-memory handshake: the certificate the server presented and the negotiated ALPN.
  fn handshake(
    server: &rustls::ServerConfig,
    offer: &[&[u8]],
    sni: &str,
  ) -> Result<(CertificateDer<'static>, Option<Vec<u8>>), rustls::Error> {
    // Like the ACME validator, the client does not judge the certificate; it reports it.
    let mut client = rustls::ClientConfig::builder_with_provider(crate::tls::provider())
      .with_protocol_versions(&[&rustls::version::TLS13])
      .expect("tls13")
      .dangerous()
      .with_custom_certificate_verifier(Arc::new(Validator))
      .with_no_client_auth();
    client.alpn_protocols = offer.iter().map(|p| p.to_vec()).collect();
    let mut client =
      rustls::ClientConnection::new(Arc::new(client), sni.to_string().try_into().expect("sni"))?;
    let mut server = rustls::ServerConnection::new(Arc::new(server.clone()))?;
    for _ in 0..16 {
      if !client.is_handshaking() && !server.is_handshaking() {
        break;
      }
      let mut flight = Vec::new();
      while client.wants_write() {
        client.write_tls(&mut flight).expect("write");
      }
      let mut rest = &flight[..];
      while !rest.is_empty() {
        server.read_tls(&mut rest).expect("read");
        server.process_new_packets()?;
      }
      let mut flight = Vec::new();
      while server.wants_write() {
        server.write_tls(&mut flight).expect("write");
      }
      let mut rest = &flight[..];
      while !rest.is_empty() {
        client.read_tls(&mut rest).expect("read");
        client.process_new_packets()?;
      }
    }
    let presented = client.peer_certificates().expect("peer certs")[0].clone();
    Ok((presented, server.alpn_protocol().map(<[u8]>::to_vec)))
  }

  #[test]
  fn config_refuses_what_tls_alpn_01_cannot_prove() {
    let dir = Path::new("/nonexistent");
    let refused = |config: AcmeConfig| {
      let error = Acme::new(config).expect_err("refused");
      assert!(matches!(error, AcmeError::Config(_)));
      error.to_string()
    };
    assert!(
      refused(AcmeConfig::new(Vec::<String>::new(), dir).agree_to_terms_of_service())
        .contains("no domains")
    );
    assert!(
      refused(AcmeConfig::new(["*.example.test"], dir).agree_to_terms_of_service())
        .contains("wildcards")
    );
    assert!(
      refused(AcmeConfig::new(["192.0.2.1"], dir).agree_to_terms_of_service())
        .contains("IP addresses")
    );
    assert!(
      refused(
        AcmeConfig::new([DOMAIN], dir)
          .directory_url("http://acme.example.test/dir")
          .agree_to_terms_of_service()
      )
      .contains("not https")
    );
    assert!(refused(AcmeConfig::new([DOMAIN], dir)).contains("terms of service"));
  }

  #[test]
  fn config_defaults_to_lets_encrypt_staging_and_normalises_domains() {
    let config = AcmeConfig::new(["App.Example.TEST", "app.example.test"], "/x");
    assert_eq!(config.directory(), LETS_ENCRYPT_STAGING);
    assert_eq!(
      config.clone().production().directory(),
      LETS_ENCRYPT_PRODUCTION
    );
    let config = config
      .contact("mailto:ops@example.test")
      .retry_after_failure(Duration::from_secs(5))
      .order_timeout(Duration::from_secs(5))
      .directory_root("/x/root.pem")
      .agree_to_terms_of_service()
      .validate()
      .expect("valid");
    assert_eq!(config.domains(), [DOMAIN.to_string()]);
    assert!(config.account_path().to_string_lossy().ends_with(".json"));
    assert_ne!(
      config.account_path(),
      config.clone().production().account_path(),
      "accounts are per directory"
    );
  }

  #[test]
  fn renewal_is_two_thirds_into_the_lifetime_minus_bounded_jitter() {
    let not_before = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let lifetime = Duration::from_secs(90 * 86_400);
    let validity = Validity {
      not_before,
      not_after: not_before + lifetime,
    };
    let two_thirds = not_before + lifetime * 2 / 3;
    assert_eq!(renewal_time(validity, 0), two_thirds);
    for seed in [1, jitter_seed(), u64::MAX] {
      let at = renewal_time(validity, seed);
      assert!(at <= two_thirds && at > two_thirds - lifetime / 20);
    }
    let instant = Validity {
      not_before,
      not_after: not_before,
    };
    assert_eq!(renewal_time(instant, 42), not_before);
  }

  #[test]
  fn resolver_answers_the_challenge_only_for_acme_tls_alpn_and_a_pending_domain() {
    let dir = tempfile::tempdir().expect("dir");
    let acme = Acme::new(config(dir.path())).expect("acme");
    let server = listener(&acme);
    assert_eq!(
      server
        .alpn_protocols
        .iter()
        .filter(|p| p.as_slice() == ACME_TLS_ALPN)
        .count(),
      1
    );
    let mut again = server.clone();
    acme.serve_tls_alpn01(&mut again);
    assert_eq!(again.alpn_protocols, server.alpn_protocols, "added once");

    let digest = [7u8; 32];
    let mut pending = ChallengeGuard {
      challenges: Arc::clone(&acme.inner.challenges),
      names: Vec::new(),
    };
    pending.insert(
      DOMAIN.to_string(),
      challenge_certificate(DOMAIN, &digest).expect("answer"),
    );
    assert!(format!("{:?}", acme.resolver()).contains("pending_challenges: 1"));

    // The validator: only acme-tls/1, SNI the domain (any case).
    let (answer, alpn) =
      handshake(&server, &[ACME_TLS_ALPN], "APP.example.test").expect("challenge handshake");
    assert!(is_acme_challenge(alpn.as_deref()));
    let (_, parsed) = x509_parser::parse_x509_certificate(answer.as_ref()).expect("x509");
    let extension = parsed
      .extensions()
      .iter()
      .find(|e| e.oid.to_id_string() == "1.3.6.1.5.5.7.1.31")
      .expect("acmeIdentifier");
    assert!(extension.critical);
    let mut expected = vec![0x04, 0x20];
    expected.extend_from_slice(&digest);
    assert_eq!(extension.value, expected.as_slice());
    let (_, names) = inspect(&answer).expect("inspect");
    assert_eq!(names, [DOMAIN.to_string()]);

    // Everyone else: the current certificate.
    let current = acme.identity().cert_sha256();
    for offer in [&[b"h2".as_slice()][..], &[b"h2", ACME_TLS_ALPN], &[]] {
      let (presented, alpn) = handshake(&server, offer, DOMAIN).expect("handshake");
      assert_eq!(crate::tls::sha256(&presented), current);
      assert!(!is_acme_challenge(alpn.as_deref()));
    }
    // acme-tls/1 for a domain without a pending challenge: no certificate.
    assert!(handshake(&server, &[ACME_TLS_ALPN], "other.example.test").is_err());
    drop(pending);
    assert!(
      handshake(&server, &[ACME_TLS_ALPN], DOMAIN).is_err(),
      "answer removed"
    );
  }

  #[test]
  fn acme_account_key_mode_0600() {
    let dir = tempfile::tempdir().expect("dir");
    let acme = Acme::new(config(dir.path())).expect("acme");
    assert_eq!(mode(dir.path()), 0o700);
    let path = acme.inner.config.account_path();
    assert!(read_private(&path).expect("read").is_none());
    write_private(&path, b"{\"id\":\"x\"}").expect("write");
    assert_eq!(mode(&path), 0o600);
    // A key file left readable by others is tightened when it is read.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let stored = read_private(&path).expect("read").expect("present");
    assert_eq!(stored.expose().as_slice(), b"{\"id\":\"x\"}");
    assert_eq!(format!("{stored:?}"), "<redacted>");
    assert_eq!(mode(&path), 0o600);
    // Overwriting keeps the mode; a stale temporary file is replaced.
    std::fs::write(path.with_extension("tmp"), b"stale").expect("stale");
    write_private(&path, b"{}").expect("rewrite");
    assert_eq!(mode(&path), 0o600);
    assert!(!path.with_extension("tmp").exists());
    assert!(matches!(
      write_private(&dir.path().join("missing/account.json"), b"{}"),
      Err(AcmeError::Io { .. })
    ));
  }

  #[tokio::test]
  async fn stored_account_that_does_not_parse_is_an_account_error() {
    let dir = tempfile::tempdir().expect("dir");
    let acme = Acme::new(config(dir.path())).expect("acme");
    write_private(&acme.inner.config.account_path(), b"not json").expect("write");
    let error = acme.account().await.map(|_| ()).expect_err("garbage");
    assert!(matches!(&error, AcmeError::Account(message) if message.contains("account-")));
  }

  #[tokio::test]
  async fn acme_failure_keeps_current_cert() {
    let dir = tempfile::tempdir().expect("dir");
    let acme = Acme::new(config(dir.path())).expect("acme");
    let before = acme.identity().cert_sha256();
    assert_eq!(acme.stats(), AcmeStats::default());
    let error = acme.issue().await.expect_err("nothing listens");
    assert!(matches!(error, AcmeError::Account(_)));
    assert_eq!(acme.identity().cert_sha256(), before);
    let stats = acme.stats();
    assert_eq!((stats.issued, stats.failed), (0, 1));
    assert!(!acme.inner.config.account_path().exists());
  }

  #[tokio::test]
  async fn spawned_renewal_retries_after_a_failure() {
    let dir = tempfile::tempdir().expect("dir");
    let acme =
      Acme::new(config(dir.path()).retry_after_failure(Duration::from_millis(10))).expect("acme");
    let task = acme.spawn();
    tokio::time::timeout(Duration::from_secs(20), async {
      while acme.stats().failed < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .expect("retried");
    drop(task);
    assert_eq!(acme.stats().issued, 0);
  }

  #[tokio::test]
  async fn a_cached_certificate_is_presented_and_not_renewed_early() {
    let dir = tempfile::tempdir().expect("dir");
    let (chain, key) = pem_for(DOMAIN, None);
    std::fs::write(dir.path().join("cert.pem"), &chain).expect("cert");
    std::fs::write(dir.path().join("key.pem"), key.expose()).expect("key");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
      dir.path().join("key.pem"),
      std::fs::Permissions::from_mode(0o640),
    )
    .expect("chmod");
    let acme = Acme::new(config(dir.path())).expect("acme");
    let cached = NodeCertificate::from_pem(chain.as_bytes(), key.expose().as_bytes())
      .expect("pem")
      .cert_sha256();
    assert_eq!(acme.identity().cert_sha256(), cached);
    assert_eq!(mode(&dir.path().join("key.pem")), 0o600);
    let stats = acme.stats();
    assert!(stats.not_after.is_some() && stats.renew_at.is_some());
    assert!(!acme.due_in().is_zero());
    // Not due: the task sleeps and makes no order.
    let task = acme.spawn();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(task);
    assert_eq!(acme.stats().failed, 0);
    let shown = format!("{acme:?}");
    assert!(
      shown.contains(DOMAIN) && !shown.contains("PRIVATE"),
      "{shown}"
    );
  }

  #[test]
  fn an_unusable_cached_certificate_falls_back_to_the_placeholder() {
    let placeholder = |chain: &str, key: &Secret<String>| {
      let dir = tempfile::tempdir().expect("dir");
      std::fs::write(dir.path().join("cert.pem"), chain).expect("cert");
      write_private(&dir.path().join("key.pem"), key.expose().as_bytes()).expect("key");
      let acme = Acme::new(config(dir.path())).expect("acme");
      let (_, names) = inspect(&acme.identity().current().cert[0]).expect("inspect");
      assert_eq!(names, [DOMAIN.to_string()]);
      acme.stats().not_after.is_none()
    };
    let (other, other_key) = pem_for("other.example.test", None);
    assert!(placeholder(&other, &other_key), "does not name the domain");
    let (expired, expired_key) = pem_for(DOMAIN, Some((2001, 1, 1)));
    assert!(placeholder(&expired, &expired_key), "expired");
    assert!(placeholder("garbage", &other_key), "does not parse");
    // No cached key: the placeholder too.
    let dir = tempfile::tempdir().expect("dir");
    assert!(
      Acme::new(config(dir.path()))
        .expect("acme")
        .stats()
        .not_after
        .is_none()
    );
  }

  #[test]
  fn install_stores_with_mode_0600_and_refuses_a_certificate_without_the_domains() {
    let dir = tempfile::tempdir().expect("dir");
    let acme = Acme::new(config(dir.path())).expect("acme");
    let before = acme.identity().cert_sha256();
    let (other, other_key) = pem_for("other.example.test", None);
    assert!(matches!(
      acme.install(other.as_bytes(), &other_key),
      Err(AcmeError::Order(message)) if message.contains(DOMAIN)
    ));
    assert_eq!(acme.identity().cert_sha256(), before);
    assert!(matches!(
      acme.install(b"garbage", &other_key),
      Err(AcmeError::Certificate(_))
    ));

    let (chain, key) = pem_for(DOMAIN, None);
    let (fingerprint, validity) = acme.install(chain.as_bytes(), &key).expect("install");
    assert_eq!(acme.identity().cert_sha256(), fingerprint);
    assert_eq!(acme.stats().not_after, Some(validity.not_after));
    assert_eq!(mode(&dir.path().join("key.pem")), 0o600);
    assert_eq!(mode(&dir.path().join("cert.pem")), 0o600);
    // A restart presents what was installed.
    let restarted = Acme::new(config(dir.path())).expect("acme");
    assert_eq!(restarted.identity().cert_sha256(), fingerprint);
  }

  #[test]
  fn inspect_refuses_what_is_not_a_certificate() {
    assert!(matches!(
      inspect(&CertificateDer::from(vec![0x30, 0x01])),
      Err(CertError::Certificate(_))
    ));
    let before_epoch = Validity {
      not_before: SystemTime::UNIX_EPOCH,
      not_after: SystemTime::UNIX_EPOCH,
    };
    assert_eq!(renewal_time(before_epoch, 1), SystemTime::UNIX_EPOCH);
    let (chain, _) = pem_for(DOMAIN, Some((1960, 1, 1)));
    let der = CertificateDer::pem_slice_iter(chain.as_bytes())
      .next()
      .expect("one")
      .expect("der");
    let (validity, _) = inspect(&der).expect("inspect");
    assert!(validity.not_after < SystemTime::UNIX_EPOCH);
    // Only DNS names count; a certificate without SANs names nothing.
    let (chain, _) = pem_for("192.0.2.7", None);
    let der = CertificateDer::pem_slice_iter(chain.as_bytes())
      .next()
      .expect("one")
      .expect("der");
    assert!(inspect(&der).expect("inspect").1.is_empty());
    let key = rcgen::KeyPair::generate().expect("key");
    let bare = rcgen::CertificateParams::default()
      .self_signed(&key)
      .expect("cert");
    assert!(inspect(bare.der()).expect("inspect").1.is_empty());
  }

  /// Records every event (formatted) and exercises the span hooks, so tests can check what
  /// reaches the logs.
  #[derive(Clone, Default)]
  struct Logs(Arc<Mutex<Vec<String>>>);

  impl Logs {
    fn text(&self) -> String {
      self.0.lock().expect("logs").join("\n")
    }
  }

  struct Fields<'a>(&'a mut String);

  impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
      self.0.push_str(&format!(" {}={value:?}", field.name()));
    }
  }

  impl tracing::Subscriber for Logs {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
      true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
      tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
      let mut line = String::new();
      event.record(&mut Fields(&mut line));
      self.0.lock().expect("logs").push(line);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
  }

  /// Base64url without padding (the encoding of stored account keys).
  fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
      let n = chunk
        .iter()
        .enumerate()
        .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
      for i in 0..=chunk.len() {
        out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
      }
    }
    out
  }

  #[tokio::test]
  async fn logs_never_carry_key_material() {
    let logs = Logs::default();
    let _guard = tracing::subscriber::set_default(logs.clone());
    let span = tracing::info_span!("acme", step = tracing::field::Empty);
    span.record("step", 1);
    span.follows_from(span.id());
    let _entered = span.enter();

    let dir = tempfile::tempdir().expect("dir");
    let (chain, key) = pem_for(DOMAIN, None);
    std::fs::write(dir.path().join("cert.pem"), "garbage").expect("cert");
    std::fs::write(dir.path().join("key.pem"), key.expose()).expect("key");
    let acme = Acme::new(config(dir.path())).expect("acme");
    acme.install(chain.as_bytes(), &key).expect("install");
    assert!(acme.issue().await.is_err());

    let text = logs.text();
    assert!(text.contains("setting mode 0600"));
    assert!(text.contains("cached ACME certificate unusable"));
    assert!(text.contains("certificate installed"));
    assert!(text.contains("keeping the current certificate"));
    let key_body = key.expose().lines().nth(1).expect("base64 line");
    assert!(!text.contains("PRIVATE KEY") && !text.contains(key_body));
  }

  #[test]
  fn generated_certificates_refuse_names_rcgen_cannot_encode() {
    let name = "b\u{fc}cher.example.test";
    assert!(matches!(
      challenge_certificate(name, &[0; 32]),
      Err(AcmeError::Certificate(_))
    ));
    assert!(matches!(
      certificate_request(&[name.to_string()]),
      Err(AcmeError::Certificate(_))
    ));
  }

  #[test]
  fn a_challenge_handshake_without_sni_gets_no_certificate() {
    let dir = tempfile::tempdir().expect("dir");
    let acme = Acme::new(config(dir.path())).expect("acme");
    assert!(handshake(&listener(&acme), &[ACME_TLS_ALPN], "127.0.0.1").is_err());
  }

  #[test]
  fn cache_and_key_files_that_cannot_be_used_are_errors_or_ignored() {
    let dir = tempfile::tempdir().expect("dir");
    // The cache directory is a file.
    let file = dir.path().join("file");
    std::fs::write(&file, b"x").expect("file");
    assert!(matches!(
      Acme::new(config(&file)),
      Err(AcmeError::Io { .. })
    ));
    // A key path that is a directory cannot be replaced.
    let occupied = dir.path().join("occupied");
    std::fs::create_dir_all(&occupied).expect("dir");
    assert!(matches!(
      write_private(&occupied, b"{}"),
      Err(AcmeError::Io { .. })
    ));
    assert!(matches!(read_private(&occupied), Err(AcmeError::Io { .. })));

    // key.pem a directory, or cert.pem missing: the placeholder.
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(cache.join("key.pem")).expect("dir");
    let acme = Acme::new(config(&cache)).expect("acme");
    assert!(acme.stats().not_after.is_none());
    let (chain, key) = pem_for(DOMAIN, None);
    assert!(matches!(
      acme.install(chain.as_bytes(), &key),
      Err(AcmeError::Io { .. })
    ));
    let cache = dir.path().join("no-cert");
    std::fs::create_dir_all(&cache).expect("dir");
    write_private(&cache.join("key.pem"), key.expose().as_bytes()).expect("key");
    assert!(
      Acme::new(config(&cache))
        .expect("acme")
        .stats()
        .not_after
        .is_none()
    );
  }

  #[tokio::test]
  async fn account_errors_keep_the_current_certificate() {
    let dir = tempfile::tempdir().expect("dir");
    // The directory root cannot be read: creating the account fails.
    let acme =
      Acme::new(config(dir.path()).directory_root(dir.path().join("missing.pem"))).expect("acme");
    assert!(matches!(acme.issue().await, Err(AcmeError::Account(_))));

    // Stored credentials: restoring them needs the directory, which does not answer.
    let credentials = serde_json::json!({
      "id": "https://127.0.0.1/acme/acct/1",
      "key_pkcs8": base64url(&rcgen::KeyPair::generate().expect("key").serialize_der()),
      "directory": acme.inner.config.directory(),
    });
    write_private(
      &acme.inner.config.account_path(),
      credentials.to_string().as_bytes(),
    )
    .expect("store");
    assert!(matches!(
      acme.account().await.map(|_| ()),
      Err(AcmeError::Account(_))
    ));
    let without_root =
      Acme::new(config(dir.path()).directory_url(acme.inner.config.directory())).expect("acme");
    assert!(matches!(
      without_root.account().await.map(|_| ()),
      Err(AcmeError::Account(_))
    ));

    // The account file is a directory.
    let other = tempfile::tempdir().expect("dir");
    let acme = Acme::new(config(other.path())).expect("acme");
    std::fs::create_dir_all(acme.inner.config.account_path()).expect("dir");
    assert!(matches!(acme.issue().await, Err(AcmeError::Io { .. })));
    assert_eq!(acme.stats().failed, 1);
  }

  #[test]
  fn base64url_matches_the_rfc_4648_vectors() {
    assert_eq!(base64url(b""), "");
    assert_eq!(base64url(b"f"), "Zg");
    assert_eq!(base64url(b"fo"), "Zm8");
    assert_eq!(base64url(b"foo"), "Zm9v");
    assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
  }

  #[test]
  fn errors_display_their_cause() {
    assert_eq!(AcmeError::Config("x".into()).to_string(), "acme: config: x");
    assert_eq!(order_error("refused").to_string(), "acme: order: refused");
    assert!(
      AcmeError::from(CertError::NoCertificate)
        .to_string()
        .contains("no certificate")
    );
  }
}
