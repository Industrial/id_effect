//! The HTTP/3 client (feature `http3`; ADR 0014 "HTTP/3", security rule C15).
//!
//! [`Http3Client`] is the platform [`HttpClient`] with HTTP/3 as an upgrade: it is built on
//! `h3` + `h3-quinn` over its own quinn client endpoint, **never on reqwest's HTTP/3** (that
//! needs a `--cfg` flag that would leak into every user's build), and keeps the reqwest
//! client as its TCP path.
//!
//! - **Upgrade only after an `Alt-Svc`.** A request to an `https` origin goes over TCP until a
//!   response from that origin advertised `Alt-Svc: h3=":<port>"` ([`AltSvcCache`]); the next
//!   request then goes over HTTP/3. `max-age` is honoured, the cache is bounded (LRU) and only
//!   same-host alternatives are used.
//! - **Fallback within 300 ms.** When the address lookup and the QUIC handshake do not complete
//!   within [`Http3Config::fallback_timeout`] (300 ms, or the connect timeout when shorter),
//!   the request goes over TCP at once, so the TCP request is never delayed by more than the
//!   fallback timer. The origin then goes to a negative cache for
//!   [`Http3Config::negative_ttl`] (5 minutes) and is not tried over HTTP/3 until it expires.
//!   A failure after the request was sent falls back too for `GET`, `PUT` and `DELETE`; a
//!   `POST` or `PATCH` that may have reached the server fails instead.
//! - **The 0.11 outbound guards apply unchanged.** The [`SsrfPolicy`] checks the URL and
//!   **every** address the host resolves to before a UDP packet is sent, and the connection
//!   uses the checked address (no second lookup, so no DNS rebinding); a refusal fails the
//!   request with [`HttpError::InvalidRequest`] (it is not retried over TCP, which would refuse
//!   it too). The total timeout ([`HttpClientConfig::timeout`] or the request's own) covers the
//!   whole exchange including the body, and a fallback's TCP request gets what is left of it.
//!   The body cap ([`HttpRequest::max_body_bytes`], default
//!   [`HttpRequest::DEFAULT_MAX_BODY_BYTES`]) is enforced on a declared `Content-Length` before
//!   any body byte is read and on the running total while reading. Retries come from the
//!   [`RetryingHttpClient`](super::RetryingHttpClient) the layer wraps around this client.
//!   Response header sections are capped at 64 KiB.
//! - **TLS.** TLS 1.3 only, 0-RTT off, ALPN `h3`, the server certificate verified against the
//!   platform CA bundle (the system bundle file: `SSL_CERT_FILE`, `NIX_SSL_CERT_FILE`, then the
//!   usual locations of Linux, the BSDs and macOS) plus
//!   [`HttpClientConfig::root_certificates`], **and the server name checked** against the
//!   origin's host. With no root found, HTTP/3 is not used (every request goes over TCP).
//! - **Network changes.** An error showing that the network changed (network unreachable or
//!   down, local address gone) clears the `Alt-Svc` cache, the negative cache and the pooled
//!   HTTP/3 connections.
//!
//! HTTP/3 connections are pooled per origin and alternative port and reused while open.
//! Connection-specific headers (`connection`, `keep-alive`, `proxy-connection`,
//! `transfer-encoding`, `upgrade`, `te`, `host`) are not sent over HTTP/3. Unlike the TCP path,
//! response bodies are not decompressed.
//!
//! ```rust,no_run
//! use id_effect_platform::http::{HttpClientConfig, ReqwestHttpClientProvider};
//!
//! // The layer builds an `Http3Client` behind the usual retrying wrapper.
//! let layer = ReqwestHttpClientProvider::layer_with::<()>(HttpClientConfig::default().http3(true));
//! # drop(layer);
//! ```

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use ::reqwest::Url;
use bytes::{Buf, Bytes};
use http::{HeaderName, HeaderValue};
use id_effect::{Effect, from_async};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

use super::alt_svc::{AltSvcCache, Origin};
use super::{
  BodySource, HttpClient, HttpClientConfig, HttpMethod, HttpRequest, HttpResponse,
  ReqwestHttpClient, SsrfPolicy, StreamingHttpResponse, buffered, streaming,
};
use crate::error::HttpError;
use crate::quic::{ALPN_H3, QuicLimits, client_config};

/// HTTP/3 settings of the client ([`HttpClientConfig::http3`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Http3Config {
  /// Whether HTTP/3 is used at all (default off: the client is opt-in).
  pub enabled: bool,
  /// How long the address lookup and the QUIC handshake may take before the request goes over
  /// TCP (default 300 ms; the connect timeout when shorter).
  pub fallback_timeout: Duration,
  /// How long an origin whose HTTP/3 attempt failed is not tried over HTTP/3 again (default
  /// 5 minutes).
  pub negative_ttl: Duration,
  /// Origins the `Alt-Svc` cache holds, least recently used evicted first (default 256).
  pub max_alt_svc_entries: usize,
}

impl Http3Config {
  /// Default [`fallback_timeout`](Self::fallback_timeout) (300 ms).
  pub const DEFAULT_FALLBACK_TIMEOUT: Duration = Duration::from_millis(300);
  /// Default [`negative_ttl`](Self::negative_ttl) (5 minutes).
  pub const DEFAULT_NEGATIVE_TTL: Duration = Duration::from_secs(5 * 60);
  /// Default [`max_alt_svc_entries`](Self::max_alt_svc_entries) (256).
  pub const DEFAULT_MAX_ALT_SVC_ENTRIES: usize = 256;
}

impl Default for Http3Config {
  /// Off, 300 ms fallback timer, 5-minute negative cache, 256 `Alt-Svc` entries.
  fn default() -> Self {
    Self {
      enabled: false,
      fallback_timeout: Self::DEFAULT_FALLBACK_TIMEOUT,
      negative_ttl: Self::DEFAULT_NEGATIVE_TTL,
      max_alt_svc_entries: Self::DEFAULT_MAX_ALT_SVC_ENTRIES,
    }
  }
}

/// Largest response header section accepted over HTTP/3 (64 KiB).
const MAX_FIELD_SECTION_SIZE: u64 = 64 * 1024;

/// Headers that are connection-specific in HTTP/1.1 and must not be sent over HTTP/3
/// (RFC 9114 §4.2); `host` is carried by `:authority`.
const CONNECTION_SPECIFIC: &[&str] = &[
  "connection",
  "keep-alive",
  "proxy-connection",
  "transfer-encoding",
  "upgrade",
  "te",
  "host",
];

/// CA bundle files tried, in order, after `SSL_CERT_FILE` and `NIX_SSL_CERT_FILE`.
const BUNDLES: &[&str] = &[
  "/etc/ssl/certs/ca-certificates.crt",
  "/etc/pki/tls/certs/ca-bundle.crt",
  "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
  "/etc/ssl/ca-bundle.pem",
  "/etc/ssl/cert.pem",
  "/usr/local/share/certs/ca-root-nss.crt",
  "/usr/local/etc/ssl/cert.pem",
];

type SendRequest = ::h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;
type RequestStream = ::h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

/// An opened response: status, headers and the capped body source.
type Opened = (u16, Vec<(String, String)>, BodySource);

/// An [`HttpError`] for an HTTP/3 transport failure.
fn h3_error(msg: impl fmt::Display) -> HttpError {
  HttpError::InvalidRequest(format!("http3: {msg}"))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An HTTP/3 response body, read frame by frame under the body cap and the request deadline.
pub(super) struct H3Body {
  stream: RequestStream,
  total: usize,
  max: usize,
  deadline: tokio::time::Instant,
}

impl H3Body {
  /// The next chunk, `None` at the end.
  pub(super) async fn pull(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
    let next = tokio::time::timeout_at(self.deadline, self.stream.recv_data()).await;
    let mut buf = match next {
      Err(_) => return Err(h3_error("response body timed out")),
      Ok(Err(e)) => return Err(h3_error(format!("response body: {e}"))),
      Ok(Ok(None)) => return Ok(None),
      Ok(Ok(Some(buf))) => buf,
    };
    let n = buf.remaining();
    let chunk = (self.total + n <= self.max).then(|| buf.copy_to_bytes(n).to_vec());
    drop(buf);
    self.total += n;
    // Over the cap the failed body is dropped, which stops the stream (h3-quinn's
    // `stop_sending` panics while a read is in flight).
    chunk.map(Some).ok_or(HttpError::BodyTooLarge {
      len: self.total,
      max: self.max,
    })
  }
}

/// Why an HTTP/3 attempt did not produce a response.
#[derive(Debug)]
enum Failure {
  /// Fails the request: an SSRF refusal, a bad header, the body cap, a timeout, or a
  /// non-idempotent request that may have reached the server.
  Fatal(HttpError),
  /// Use TCP instead, and exclude the origin (unless the network changed).
  Fallback {
    reason: String,
    network_change: bool,
  },
}

impl Failure {
  fn fallback(reason: impl fmt::Display) -> Self {
    Self::Fallback {
      reason: reason.to_string(),
      network_change: false,
    }
  }

  fn into_error(self) -> HttpError {
    match self {
      Self::Fatal(e) => e,
      Self::Fallback { reason, .. } => h3_error(reason),
    }
  }
}

/// Where an exchange failed.
enum Stage {
  /// Opening the request stream: nothing reached the server's application.
  Open(String),
  /// After the request was (perhaps partly) sent.
  Sent(String),
}

/// Whether `e` shows that the local network changed.
fn is_network_change_io(e: &io::Error) -> bool {
  matches!(
    e.kind(),
    io::ErrorKind::NetworkUnreachable
      | io::ErrorKind::NetworkDown
      | io::ErrorKind::AddrNotAvailable
  )
}

/// Whether `e` or any error in its source chain shows that the local network changed.
fn is_network_change(e: &(dyn Error + 'static)) -> bool {
  let mut next = Some(e);
  while let Some(e) = next {
    if e
      .downcast_ref::<io::Error>()
      .is_some_and(is_network_change_io)
    {
      return true;
    }
    next = e.source();
  }
  false
}

/// The bundle files to try for the platform roots.
fn bundle_candidates() -> Vec<PathBuf> {
  ["SSL_CERT_FILE", "NIX_SSL_CERT_FILE"]
    .into_iter()
    .filter_map(std::env::var_os)
    .map(PathBuf::from)
    .chain(BUNDLES.iter().map(PathBuf::from))
    .collect()
}

/// The usable certificates of a PEM bundle (unparsable ones are skipped).
fn roots_from_pem(pem: &[u8]) -> rustls::RootCertStore {
  let mut roots = rustls::RootCertStore::empty();
  roots.add_parsable_certificates(CertificateDer::pem_slice_iter(pem).filter_map(Result::ok));
  roots
}

/// The roots of the first readable, non-empty bundle among `candidates`.
fn first_bundle(candidates: &[PathBuf]) -> rustls::RootCertStore {
  candidates
    .iter()
    .filter_map(|path| std::fs::read(path).ok())
    .map(|pem| roots_from_pem(&pem))
    .find(|roots| !roots.is_empty())
    .unwrap_or_else(rustls::RootCertStore::empty)
}

/// The platform CA roots (read once per process).
fn platform_roots() -> rustls::RootCertStore {
  static ROOTS: OnceLock<rustls::RootCertStore> = OnceLock::new();
  ROOTS
    .get_or_init(|| first_bundle(&bundle_candidates()))
    .clone()
}

/// The quinn client config for HTTP/3 under `config`, or `None` when HTTP/3 is off or no CA
/// root is available.
fn quic_config(config: &HttpClientConfig) -> Option<quinn::ClientConfig> {
  if !config.http3.enabled {
    return None;
  }
  quic_config_with(config, platform_roots())
}

/// [`quic_config`] with `roots` as the platform roots.
fn quic_config_with(
  config: &HttpClientConfig,
  mut roots: rustls::RootCertStore,
) -> Option<quinn::ClientConfig> {
  roots.add_parsable_certificates(
    config
      .root_certificates
      .iter()
      .map(|der| CertificateDer::from(der.clone())),
  );
  if roots.is_empty() {
    return None;
  }
  let tls = rustls::ClientConfig::builder_with_provider(crate::tls::provider())
    .with_protocol_versions(&[&rustls::version::TLS13])
    .ok()?
    .with_root_certificates(roots)
    .with_no_client_auth();
  client_config(tls, &[ALPN_H3], &QuicLimits::default()).ok()
}

/// The HTTP/3 request for `req` (header validation happens here, before any I/O).
fn h3_request(req: &HttpRequest, url: &Url) -> Result<http::Request<()>, Failure> {
  let method = match req.method {
    HttpMethod::Get => http::Method::GET,
    HttpMethod::Post => http::Method::POST,
    HttpMethod::Put => http::Method::PUT,
    HttpMethod::Delete => http::Method::DELETE,
    HttpMethod::Patch => http::Method::PATCH,
  };
  let mut url = url.clone();
  url.set_fragment(None);
  let mut builder = http::Request::builder().method(method).uri(url.as_str());
  for (k, v) in &req.headers {
    let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| {
      Failure::Fatal(HttpError::InvalidRequest(format!(
        "bad header name {k:?}: {e}"
      )))
    })?;
    let value = HeaderValue::from_str(v).map_err(|e| {
      Failure::Fatal(HttpError::InvalidRequest(format!(
        "bad header value for {k:?}: {e}"
      )))
    })?;
    if !CONNECTION_SPECIFIC.contains(&name.as_str()) {
      builder = builder.header(name, value);
    }
  }
  builder
    .body(())
    .map_err(|e| Failure::Fatal(h3_error(format!("request: {e}"))))
}

/// Whether `method` may be sent again after it may have reached the server.
fn idempotent(method: &HttpMethod) -> bool {
  matches!(
    method,
    HttpMethod::Get | HttpMethod::Put | HttpMethod::Delete
  )
}

/// Sends `request` (and `body`) on a new stream of `send` and reads the response head.
async fn exchange(
  send: &mut SendRequest,
  request: http::Request<()>,
  body: Option<&[u8]>,
) -> Result<(http::Response<()>, RequestStream), Stage> {
  let mut stream = send
    .send_request(request)
    .await
    .map_err(|e| Stage::Open(e.to_string()))?;
  let sent = |e: ::h3::error::StreamError| Stage::Sent(e.to_string());
  if let Some(body) = body {
    stream
      .send_data(Bytes::copy_from_slice(body))
      .await
      .map_err(sent)?;
  }
  stream.finish().await.map_err(sent)?;
  let response = stream.recv_response().await.map_err(sent)?;
  Ok((response, stream))
}

/// An open HTTP/3 connection.
struct Pooled {
  conn: quinn::Connection,
  send: SendRequest,
}

/// The client endpoints, one per address family, bound on first use.
#[derive(Default)]
struct Endpoints {
  v4: Option<quinn::Endpoint>,
  v6: Option<quinn::Endpoint>,
}

type PoolKey = (Origin, u16);

struct Inner {
  ssrf: SsrfPolicy,
  settings: Http3Config,
  timeout: Duration,
  connect_timeout: Duration,
  alt_svc: AltSvcCache,
  quic: Option<quinn::ClientConfig>,
  endpoints: Mutex<Endpoints>,
  pool: Mutex<HashMap<PoolKey, Pooled>>,
}

impl Inner {
  /// The client endpoint for `target`'s address family.
  fn endpoint(&self, target: SocketAddr) -> io::Result<quinn::Endpoint> {
    let mut endpoints = lock(&self.endpoints);
    let (slot, bind) = if target.is_ipv4() {
      (
        &mut endpoints.v4,
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
      )
    } else {
      (
        &mut endpoints.v6,
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
      )
    };
    if let Some(endpoint) = slot {
      return Ok(endpoint.clone());
    }
    let endpoint = quinn::Endpoint::client(bind)?;
    *slot = Some(endpoint.clone());
    Ok(endpoint)
  }

  /// Forgets what the network taught: alternatives, exclusions, connections and sockets.
  fn forget_network(&self) {
    self.alt_svc.clear();
    lock(&self.pool).clear();
    *lock(&self.endpoints) = Endpoints::default();
  }

  /// The one address of `host` to dial, after the SSRF check of every address it resolves to.
  async fn resolve(&self, host: &str, port: u16) -> Result<SocketAddr, Failure> {
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
      Ok(ip) => vec![SocketAddr::new(ip, port)],
      Err(_) => tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| Failure::fallback(format!("dns: {e}")))?
        .collect(),
    };
    self
      .ssrf
      .check_resolved(host, addrs.iter().map(SocketAddr::ip))
      .map_err(|blocked| Failure::Fatal(HttpError::InvalidRequest(blocked.to_string())))?;
    addrs
      .first()
      .copied()
      .ok_or_else(|| Failure::fallback(format!("dns: {host} has no address")))
  }

  /// A QUIC + HTTP/3 connection to `host` at `addr`, its server certificate verified for
  /// `host`.
  async fn connect(
    &self,
    config: quinn::ClientConfig,
    host: &str,
    addr: SocketAddr,
  ) -> Result<Pooled, Failure> {
    let endpoint = self.endpoint(addr).map_err(|e| Failure::Fallback {
      reason: format!("udp socket: {e}"),
      network_change: is_network_change_io(&e),
    })?;
    let conn = endpoint
      .connect_with(config, addr, host)
      .map_err(|e| Failure::fallback(format!("connect: {e}")))?
      .await
      .map_err(|e| Failure::fallback(format!("handshake: {e}")))?;
    let (mut driver, send) = ::h3::client::builder()
      .max_field_section_size(MAX_FIELD_SECTION_SIZE)
      .build::<_, _, Bytes>(h3_quinn::Connection::new(conn.clone()))
      .await
      .map_err(|e| Failure::fallback(format!("h3 setup: {e}")))?;
    tokio::spawn(async move {
      let _closed = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });
    Ok(Pooled { conn, send })
  }

  /// A pooled, still open connection for `key`.
  fn pooled(&self, key: &PoolKey) -> Option<SendRequest> {
    let mut pool = lock(&self.pool);
    match pool.get(key) {
      Some(p) if p.conn.close_reason().is_none() => Some(p.send.clone()),
      Some(_) => {
        pool.remove(key);
        None
      }
      None => None,
    }
  }

  /// A new connection for `key`, within the fallback timer, added to the pool.
  async fn dial(&self, config: quinn::ClientConfig, key: &PoolKey) -> Result<SendRequest, Failure> {
    let (origin, port) = key;
    let budget = self.settings.fallback_timeout.min(self.connect_timeout);
    let connecting = async {
      let addr = self.resolve(origin.host(), *port).await?;
      self.connect(config, origin.host(), addr).await
    };
    let pooled = tokio::time::timeout(budget, connecting)
      .await
      .map_err(|_| Failure::fallback(format!("handshake did not complete within {budget:?}")))??;
    let send = pooled.send.clone();
    lock(&self.pool).insert(key.clone(), pooled);
    Ok(send)
  }

  /// One request over HTTP/3 to `origin`'s alternative on UDP `port`, by `deadline`.
  async fn try_h3(
    &self,
    req: &HttpRequest,
    url: &Url,
    origin: &Origin,
    port: u16,
    deadline: tokio::time::Instant,
  ) -> Result<Opened, Failure> {
    let config = self
      .quic
      .clone()
      .ok_or_else(|| Failure::fallback("HTTP/3 is off or has no CA roots"))?;
    let request = h3_request(req, url)?;
    let key = (origin.clone(), port);
    let body = req.body.as_deref();
    let (mut send, reused) = match self.pooled(&key) {
      Some(send) => (send, true),
      None => (self.dial(config.clone(), &key).await?, false),
    };
    let timed_out = || Failure::Fatal(h3_error("request timed out"));
    let mut outcome = tokio::time::timeout_at(deadline, exchange(&mut send, request, body))
      .await
      .map_err(|_| timed_out())?;
    if let (Err(Stage::Open(_)), true) = (&outcome, reused) {
      // A pooled connection the server already closed: dial once more.
      lock(&self.pool).remove(&key);
      let mut send = self.dial(config, &key).await?;
      outcome = tokio::time::timeout_at(deadline, exchange(&mut send, h3_request(req, url)?, body))
        .await
        .map_err(|_| timed_out())?;
    }
    let (response, stream) = match outcome {
      Ok(ok) => ok,
      Err(Stage::Open(e)) => return Err(Failure::fallback(format!("request: {e}"))),
      Err(Stage::Sent(e)) if idempotent(&req.method) => {
        return Err(Failure::fallback(format!("request: {e}")));
      }
      Err(Stage::Sent(e)) => return Err(Failure::Fatal(h3_error(format!("request: {e}")))),
    };
    let max = req.max_body();
    let declared = response
      .headers()
      .get(http::header::CONTENT_LENGTH)
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.parse::<u64>().ok());
    if let Some(len) = declared.filter(|len| *len > max as u64) {
      // Dropping the stream stops it before any body byte is read.
      return Err(Failure::Fatal(HttpError::BodyTooLarge {
        len: usize::try_from(len).unwrap_or(usize::MAX),
        max,
      }));
    }
    let headers = response
      .headers()
      .iter()
      .filter_map(|(k, v)| {
        v.to_str()
          .ok()
          .map(|s| (k.as_str().to_string(), s.to_string()))
      })
      .collect();
    let body = H3Body {
      stream,
      total: 0,
      max,
      deadline,
    };
    Ok((
      response.status().as_u16(),
      headers,
      BodySource::H3(Box::new(body)),
    ))
  }

  /// After an HTTP/3 attempt fell back: forget the network when it changed, else exclude
  /// `origin` for the negative TTL.
  fn fall_back(&self, origin: &Origin, network_change: bool) {
    if network_change {
      self.forget_network();
    } else {
      self.alt_svc.mark_failed(origin, Instant::now());
    }
  }

  /// After a TCP failure: forget the network when the error shows it changed.
  fn after_tcp_error(&self, e: &(dyn Error + 'static)) {
    if is_network_change(e) {
      self.forget_network();
    }
  }

  /// Records the `Alt-Svc` headers of a response from `origin`.
  fn learn(&self, origin: &Origin, headers: &[(String, String)]) {
    let now = Instant::now();
    for (_, value) in headers
      .iter()
      .filter(|(k, _)| k.eq_ignore_ascii_case("alt-svc"))
    {
      self.alt_svc.record(origin, value, now);
    }
  }
}

/// The platform [`HttpClient`] with HTTP/3 (see the [module docs](self)): TCP through a
/// [`ReqwestHttpClient`] built from the same config, HTTP/3 through `h3` + `h3-quinn` once an
/// origin advertised it. Cloning shares the caches and connections.
#[derive(Clone)]
pub struct Http3Client {
  tcp: ReqwestHttpClient,
  inner: Arc<Inner>,
}

impl fmt::Debug for Http3Client {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Http3Client")
      .field("settings", &self.inner.settings)
      .field("http3_available", &self.http3_available())
      .field("alt_svc_entries", &self.inner.alt_svc.len())
      .finish_non_exhaustive()
  }
}

impl Http3Client {
  /// Build with `config`: its timeouts, [`SsrfPolicy`], redirect limit and roots for both
  /// paths, and its [`Http3Config`] (HTTP/3 is used only when
  /// [`enabled`](Http3Config::enabled), [`HttpClientConfig::http3`]). The config's retry
  /// policy is the layer's.
  ///
  /// # Panics
  ///
  /// As [`ReqwestHttpClient::from_config`].
  pub fn from_config(config: &HttpClientConfig) -> Self {
    Self {
      tcp: ReqwestHttpClient::from_config(config),
      inner: Arc::new(Inner {
        ssrf: config.ssrf.clone(),
        settings: config.http3.clone(),
        timeout: config.timeout,
        connect_timeout: config.connect_timeout,
        alt_svc: AltSvcCache::new(config.http3.max_alt_svc_entries, config.http3.negative_ttl),
        quic: quic_config(config),
        endpoints: Mutex::new(Endpoints::default()),
        pool: Mutex::new(HashMap::new()),
      }),
    }
  }

  /// The `Alt-Svc` cache (inspect it, or seed an origin known to serve HTTP/3).
  pub fn alt_svc(&self) -> &AltSvcCache {
    &self.inner.alt_svc
  }

  /// Whether HTTP/3 can be used: it is enabled and a CA root was found.
  pub fn http3_available(&self) -> bool {
    self.inner.quic.is_some()
  }

  /// Executes `req` over HTTP/3 only, on UDP `port` of the URL's host, without consulting the
  /// `Alt-Svc` cache and without a TCP fallback. Every guard of the module docs applies; a
  /// failure is an [`HttpError`] (a transport failure is [`HttpError::InvalidRequest`] starting
  /// `http3:`).
  pub fn execute_h3(&self, req: HttpRequest, port: u16) -> Effect<HttpResponse, HttpError> {
    let inner = Arc::clone(&self.inner);
    from_async(move || async move {
      let url = Url::parse(&req.url)
        .map_err(|e| HttpError::InvalidRequest(format!("{:?} is not a URL: {e}", req.url)))?;
      inner
        .ssrf
        .check_parsed(&url)
        .map_err(|blocked| HttpError::InvalidRequest(blocked.to_string()))?;
      let origin =
        Origin::from_url(&url).ok_or_else(|| h3_error("only https URLs go over HTTP/3"))?;
      let deadline = tokio::time::Instant::now() + req.timeout.unwrap_or(inner.timeout);
      let opened = inner
        .try_h3(&req, &url, &origin, port, deadline)
        .await
        .map_err(Failure::into_error)?;
      buffered(opened).await
    })
  }

  /// Sends `req` over HTTP/3 when its origin advertised it, else (or on a fallback) over TCP.
  async fn open(&self, mut req: HttpRequest) -> Result<Opened, HttpError> {
    let inner = &self.inner;
    let url = Url::parse(&req.url).ok();
    let origin = url
      .as_ref()
      .and_then(Origin::from_url)
      .filter(|_| inner.quic.is_some());
    if let (Some(url), Some(origin)) = (&url, &origin) {
      inner
        .ssrf
        .check_parsed(url)
        .map_err(|blocked| HttpError::InvalidRequest(blocked.to_string()))?;
      if let Some(port) = inner.alt_svc.lookup(origin, Instant::now()) {
        let started = Instant::now();
        let total = req.timeout.unwrap_or(inner.timeout);
        let deadline = tokio::time::Instant::now() + total;
        match inner.try_h3(&req, url, origin, port, deadline).await {
          Ok(opened) => {
            inner.learn(origin, &opened.1);
            return Ok(opened);
          }
          Err(Failure::Fatal(e)) => return Err(e),
          Err(Failure::Fallback { network_change, .. }) => {
            inner.fall_back(origin, network_change);
            req.timeout = Some(total.saturating_sub(started.elapsed()));
          }
        }
      }
    }
    let opened = self
      .tcp
      .open(&req)
      .await
      .inspect_err(|e| inner.after_tcp_error(e))?;
    if let Some(origin) = &origin {
      inner.learn(origin, &opened.1);
    }
    Ok(opened)
  }
}

impl HttpClient for Http3Client {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    let client = self.clone();
    from_async(move || async move { buffered(client.open(req).await?).await })
  }

  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    let client = self.clone();
    from_async(move || async move { Ok(streaming(client.open(req).await?)) })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Debug)]
  struct Wrapped(io::Error);

  impl fmt::Display for Wrapped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(f, "wrapped")
    }
  }

  impl Error for Wrapped {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
      Some(&self.0)
    }
  }

  #[test]
  fn network_change_errors_are_found_in_the_source_chain() {
    for kind in [
      io::ErrorKind::NetworkUnreachable,
      io::ErrorKind::NetworkDown,
      io::ErrorKind::AddrNotAvailable,
    ] {
      assert!(is_network_change(&Wrapped(io::Error::from(kind))));
    }
    assert!(!is_network_change(&Wrapped(io::Error::from(
      io::ErrorKind::ConnectionRefused
    ))));
    assert!(!is_network_change(&HttpError::InvalidRequest("x".into())));
  }

  #[test]
  fn roots_come_from_the_first_usable_bundle() {
    let dir = tempfile::tempdir().expect("dir");
    let empty = dir.path().join("empty.pem");
    std::fs::write(&empty, b"no certificates here").expect("write");
    let good = dir.path().join("good.pem");
    let cert = rcgen::generate_simple_self_signed(vec!["ca.test".to_string()]).expect("cert");
    // A garbage block is skipped, the real certificate kept.
    let pem = format!(
      "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n{}",
      cert.cert.pem()
    );
    std::fs::write(&good, pem).expect("write");
    let missing = dir.path().join("missing.pem");
    assert_eq!(first_bundle(&[missing.clone(), empty, good]).len(), 1);
    assert!(first_bundle(&[missing]).is_empty());
    assert!(bundle_candidates().len() >= BUNDLES.len());
  }

  #[test]
  fn requests_drop_connection_specific_headers_and_check_the_rest() {
    let url = Url::parse("https://a.test/p?q=1#frag").expect("url");
    let mut req = HttpRequest::post("https://a.test/p", b"x".to_vec());
    req.headers = vec![
      ("Connection".into(), "close".into()),
      ("Host".into(), "a.test".into()),
      ("X-Trace".into(), "1".into()),
    ];
    let built = h3_request(&req, &url).expect("request");
    assert_eq!(built.method(), http::Method::POST);
    assert_eq!(built.uri(), "https://a.test/p?q=1");
    assert_eq!(built.headers().len(), 1);
    assert_eq!(built.headers()["x-trace"], "1");
    for method in [
      HttpMethod::Get,
      HttpMethod::Put,
      HttpMethod::Delete,
      HttpMethod::Patch,
    ] {
      let req = HttpRequest {
        method: method.clone(),
        ..HttpRequest::get("https://a.test/")
      };
      assert_eq!(
        h3_request(&req, &url).expect("request").method().as_str(),
        format!("{method:?}").to_uppercase()
      );
    }
    let mut bad = HttpRequest::get("https://a.test/");
    bad.headers = vec![("bad name".into(), "v".into())];
    assert!(matches!(
      h3_request(&bad, &url),
      Err(Failure::Fatal(HttpError::InvalidRequest(m))) if m.contains("bad header name")
    ));
    bad.headers = vec![("x".into(), "line\nbreak".into())];
    assert!(matches!(
      h3_request(&bad, &url),
      Err(Failure::Fatal(HttpError::InvalidRequest(m))) if m.contains("bad header value")
    ));
  }

  #[test]
  fn idempotency_and_failure_errors() {
    assert!(idempotent(&HttpMethod::Get));
    assert!(!idempotent(&HttpMethod::Post));
    assert!(!idempotent(&HttpMethod::Patch));
    assert!(matches!(
      Failure::fallback("gone").into_error(),
      HttpError::InvalidRequest(m) if m == "http3: gone"
    ));
    assert!(matches!(
      Failure::Fatal(HttpError::BodyTooLarge { len: 2, max: 1 }).into_error(),
      HttpError::BodyTooLarge { .. }
    ));
  }

  #[test]
  fn config_defaults_and_availability() {
    let defaults = Http3Config::default();
    assert!(!defaults.enabled);
    assert_eq!(defaults.fallback_timeout, Duration::from_millis(300));
    assert_eq!(defaults.negative_ttl, Duration::from_secs(300));
    assert_eq!(defaults.max_alt_svc_entries, 256);
    let off = Http3Client::from_config(&HttpClientConfig::default());
    assert!(!off.http3_available());
    assert!(format!("{off:?}").contains("Http3Client"));
    // An explicit root makes HTTP/3 available even without a platform bundle.
    let cert = rcgen::generate_simple_self_signed(vec!["ca.test".to_string()]).expect("cert");
    let on = Http3Client::from_config(
      &HttpClientConfig::default()
        .add_root_certificate_der(cert.cert.der().to_vec())
        .http3(true),
    );
    assert!(on.http3_available());
    assert!(on.alt_svc().is_empty());
  }

  #[test]
  fn no_roots_means_no_http3() {
    let on = HttpClientConfig::default().http3(true);
    assert!(quic_config_with(&on, rustls::RootCertStore::empty()).is_none());
    let cert = rcgen::generate_simple_self_signed(vec!["ca.test".to_string()]).expect("cert");
    let with_root = on.add_root_certificate_der(cert.cert.der().to_vec());
    assert!(quic_config_with(&with_root, rustls::RootCertStore::empty()).is_some());
  }

  #[test]
  fn fallbacks_exclude_the_origin_unless_the_network_changed() {
    let client = Http3Client::from_config(&HttpClientConfig::default().http3(true));
    let inner = &client.inner;
    let origin = Origin::new("a.test", 443);
    let now = Instant::now();
    inner.fall_back(&origin, false);
    assert!(inner.alt_svc.is_negative(&origin, now));
    inner.fall_back(&origin, true);
    assert!(!inner.alt_svc.is_negative(&origin, now), "cleared");

    inner
      .alt_svc
      .insert(&origin, 443, Duration::from_secs(60), now);
    let refused = Wrapped(io::Error::from(io::ErrorKind::ConnectionRefused));
    assert_eq!(refused.to_string(), "wrapped");
    inner.after_tcp_error(&refused);
    assert_eq!(
      inner.alt_svc.len(),
      1,
      "a refused connection is not a network change"
    );
    inner.after_tcp_error(&Wrapped(io::Error::from(io::ErrorKind::NetworkDown)));
    assert!(inner.alt_svc.is_empty());
  }

  #[tokio::test]
  async fn forget_network_drops_sockets_and_caches() {
    let client = Http3Client::from_config(&HttpClientConfig::default().http3(true));
    let inner = &client.inner;
    let v4 = inner
      .endpoint("127.0.0.1:9".parse().expect("addr"))
      .expect("v4 endpoint");
    let again = inner
      .endpoint("127.0.0.1:10".parse().expect("addr"))
      .expect("v4 endpoint");
    assert_eq!(v4.local_addr().ok(), again.local_addr().ok(), "reused");
    let origin = Origin::new("a.test", 443);
    inner
      .alt_svc
      .insert(&origin, 443, Duration::from_secs(60), Instant::now());
    inner.forget_network();
    assert!(inner.alt_svc.is_empty());
    assert!(lock(&inner.endpoints).v4.is_none());
  }
}
