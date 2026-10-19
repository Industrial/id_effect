//! Portable HTTP client ([`HttpClient`]) and reqwest-backed [`ReqwestHttpClient`].
//!
//! The capability is [`HttpClientService`]; [`ReqwestHttpClientProvider::layer`] (also spelled
//! [`provide_reqwest_http_client`]) and [`provide_http_client`] are the [`Layer`]s that provide
//! it ([`HttpClientEnv`]).
//!
//! The reqwest layer applies client-wide defaults, configured once ([`HttpClientConfig`],
//! [`ReqwestHttpClientProvider::layer_with`]): a 10 s connect timeout, a 30 s total timeout
//! (a request's own [`HttpRequest::timeout`] replaces it), a [`RetryPolicy`] retrying
//! idempotent methods on connect errors, `5xx` and `429` ([`RetryingHttpClient`]), the SSRF guard
//! ([`SsrfPolicy`]: no loopback, private, link-local or metadata destinations, `http`/`https`
//! only; every resolved address is checked) and no redirects followed
//! ([`HttpClientConfig::follow_redirects`] opts in, each hop re-checked).
//!
//! The reqwest client needs a tokio runtime with its IO driver enabled, such as
//! `id_effect_tokio::TokioRuntime`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use ::reqwest::header::{HeaderName, HeaderValue};
use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, from_async, hlist, need, sync};
use tokio::sync::Mutex;

use crate::error::HttpError;

pub mod retry;
pub mod ssrf;

pub use retry::{RetryCause, RetryPolicy, RetryingHttpClient};
pub use ssrf::{IpCidr, SsrfBlocked, SsrfPolicy};

/// HTTP verb for [`HttpRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpMethod {
  /// GET
  Get,
  /// POST
  Post,
  /// PUT
  Put,
  /// DELETE
  Delete,
  /// PATCH
  Patch,
}

/// Minimal portable HTTP request description.
#[derive(Clone, Debug)]
pub struct HttpRequest {
  /// HTTP method.
  pub method: HttpMethod,
  /// Full URL string.
  pub url: String,
  /// `(name, value)` header pairs.
  pub headers: Vec<(String, String)>,
  /// Optional request body.
  pub body: Option<Vec<u8>>,
  /// Per-request timeout (applied on top of client defaults).
  pub timeout: Option<Duration>,
  /// Maximum response body bytes to buffer in memory (default [`HttpRequest::DEFAULT_MAX_BODY_BYTES`]).
  pub max_body_bytes: Option<usize>,
}

impl HttpRequest {
  /// Default cap for buffered response bodies (10 MiB).
  pub const DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

  /// GET without body.
  #[inline]
  pub fn get(url: impl Into<String>) -> Self {
    Self {
      method: HttpMethod::Get,
      url: url.into(),
      headers: Vec::new(),
      body: None,
      timeout: None,
      max_body_bytes: None,
    }
  }

  /// POST with body bytes.
  #[inline]
  pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
    Self {
      method: HttpMethod::Post,
      url: url.into(),
      headers: Vec::new(),
      body: Some(body),
      timeout: None,
      max_body_bytes: None,
    }
  }

  fn max_body(&self) -> usize {
    self.max_body_bytes.unwrap_or(Self::DEFAULT_MAX_BODY_BYTES)
  }
}

/// Buffered HTTP response (MVP — full body in memory).
#[derive(Clone, Debug)]
pub struct HttpResponse {
  /// Status code.
  pub status: u16,
  /// `(name, value)` headers (lowercased names not guaranteed).
  pub headers: Vec<(String, String)>,
  /// Response body bytes.
  pub body: Vec<u8>,
}

/// Where an [`HttpBody`] pulls its chunks from.
enum BodySource {
  /// Ready chunks (test doubles, buffered bodies).
  Chunks(VecDeque<Vec<u8>>),
  /// A live reqwest response, read chunk by chunk with a running size cap.
  Reqwest {
    resp: ::reqwest::Response,
    total: usize,
    max: usize,
  },
  /// A live response whose `Content-Length` is over the cap: fails on the first read, before
  /// any body byte is read.
  TooLarge { len: usize, max: usize },
  /// Exhausted (or failed: a failed body yields no further chunks).
  Done,
}

impl BodySource {
  /// The next chunk, `None` at the end.
  async fn pull(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
    let out = match self {
      Self::Chunks(q) => return Ok(q.pop_front()),
      Self::Done => return Ok(None),
      Self::TooLarge { len, max } => Err(HttpError::BodyTooLarge {
        len: *len,
        max: *max,
      }),
      Self::Reqwest { resp, total, max } => match resp.chunk().await {
        Ok(Some(bytes)) => {
          *total += bytes.len();
          if *total > *max {
            Err(HttpError::BodyTooLarge {
              len: *total,
              max: *max,
            })
          } else {
            return Ok(Some(bytes.to_vec()));
          }
        }
        Ok(None) => Ok(None),
        Err(e) => Err(HttpError::from(e)),
      },
    };
    *self = Self::Done;
    out
  }

  /// The rest of the body in one buffer (stops at the first failed chunk).
  async fn read_to_end(&mut self) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    while let Some(chunk) = self.pull().await? {
      out.extend_from_slice(&chunk);
    }
    Ok(out)
  }
}

/// A pull-based HTTP response body: successive byte chunks read on demand as effects.
///
/// Reading is lazy and backpressured: a live reqwest body reads one network chunk per
/// [`next_chunk`](Self::next_chunk), and [`collect`](Self::collect) reads to the end. The
/// request's [`max_body_bytes`](HttpRequest::max_body_bytes) cap is enforced as chunks arrive,
/// and a declared `Content-Length` over it fails the first read before any byte is read
/// ([`HttpError::BodyTooLarge`]). Until `id_effect` has a `Stream`, this is the streaming
/// body type.
pub struct HttpBody {
  src: Arc<Mutex<BodySource>>,
}

impl std::fmt::Debug for HttpBody {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("HttpBody").finish_non_exhaustive()
  }
}

impl HttpBody {
  fn from_source(src: BodySource) -> Self {
    Self {
      src: Arc::new(Mutex::new(src)),
    }
  }

  /// A body with no chunks.
  #[inline]
  pub fn empty() -> Self {
    Self::from_source(BodySource::Done)
  }

  /// A body yielding `chunks` in order (test doubles).
  #[inline]
  pub fn from_chunks(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
    Self::from_source(BodySource::Chunks(chunks.into_iter().collect()))
  }

  /// A buffered body split into chunks of `chunk_size` bytes (at least 1).
  #[inline]
  pub fn from_bytes(body: &[u8], chunk_size: usize) -> Self {
    Self::from_chunks(body.chunks(chunk_size.max(1)).map(<[u8]>::to_vec))
  }

  fn from_reqwest(resp: ::reqwest::Response, max: usize) -> Self {
    Self::from_source(reqwest_source(resp, max))
  }

  /// The next chunk, or `None` once the body is exhausted.
  pub fn next_chunk(&self) -> Effect<Option<Vec<u8>>, HttpError> {
    let src = Arc::clone(&self.src);
    from_async(move || async move { src.lock().await.pull().await })
  }

  /// Reads the rest of the body into one buffer.
  pub fn collect(self) -> Effect<Vec<u8>, HttpError> {
    from_async(move || async move { self.src.lock().await.read_to_end().await })
  }
}

/// The body source for `resp` under a `max`-byte cap: rejected up front when the declared
/// `Content-Length` is over `max`, otherwise read chunk by chunk with a running total.
fn reqwest_source(resp: ::reqwest::Response, max: usize) -> BodySource {
  match resp.content_length() {
    Some(declared) if declared > max as u64 => BodySource::TooLarge {
      len: usize::try_from(declared).unwrap_or(usize::MAX),
      max,
    },
    _ => BodySource::Reqwest {
      resp,
      total: 0,
      max,
    },
  }
}

/// HTTP response whose body is read chunk by chunk ([`HttpBody`]).
#[derive(Debug)]
pub struct StreamingHttpResponse {
  /// Status code.
  pub status: u16,
  /// `(name, value)` headers.
  pub headers: Vec<(String, String)>,
  /// Chunked response body.
  pub body: HttpBody,
}

/// A buffered [`HttpResponse`]'s body as an [`HttpBody`] of `chunk_size`-byte chunks.
#[inline]
pub fn response_body_stream(resp: &HttpResponse, chunk_size: usize) -> HttpBody {
  HttpBody::from_bytes(&resp.body, chunk_size)
}

/// Capability: execute portable HTTP requests as [`Effect`] values.
pub trait HttpClient: Send + Sync + 'static {
  /// Execute `req` and return a buffered response.
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError>;

  /// Execute `req` and return a response whose body is read on demand.
  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError>;
}

/// [`::reqwest::Client`]-backed [`HttpClient`].
///
/// A client built here ([`from_config`](Self::from_config), [`default_client`](Self::default_client),
/// [`with_timeouts`](Self::with_timeouts)) enforces its config's [`SsrfPolicy`] and redirect
/// limit; a request it refuses fails with [`HttpError::InvalidRequest`] naming the policy.
#[derive(Clone, Debug)]
pub struct ReqwestHttpClient {
  inner: ::reqwest::Client,
  ssrf: Option<Arc<SsrfPolicy>>,
}

impl ReqwestHttpClient {
  /// Wrap an existing [`::reqwest::Client`], used as configured: no SSRF guard and its own
  /// redirect policy (reqwest's default follows up to 10). Prefer [`from_config`](Self::from_config).
  #[inline]
  pub fn new(client: ::reqwest::Client) -> Self {
    Self {
      inner: client,
      ssrf: None,
    }
  }

  /// Build with [`HttpClientConfig::default`]'s timeouts, SSRF guard and redirect limit (the
  /// config's retry policy is the layer's, [`RetryingHttpClient`]).
  ///
  /// # Panics
  ///
  /// As [`::reqwest::Client::new`]: if the TLS backend cannot be initialised.
  #[inline]
  pub fn default_client() -> Self {
    Self::from_config(&HttpClientConfig::default())
  }

  /// Build with a `connect` timeout and a `total` per-request timeout (a request's own
  /// [`HttpRequest::timeout`] replaces `total`), and the default SSRF guard and redirect limit.
  ///
  /// # Panics
  ///
  /// As [`::reqwest::Client::new`]: if the TLS backend cannot be initialised.
  pub fn with_timeouts(connect: Duration, total: Duration) -> Self {
    Self::from_config(
      &HttpClientConfig::default()
        .with_connect_timeout(connect)
        .with_timeout(total),
    )
  }

  /// Build with `config`'s timeouts, [`SsrfPolicy`] (checked on the URL, on every resolved
  /// address by the client's DNS resolver, and on every redirect hop) and redirect limit.
  ///
  /// # Panics
  ///
  /// As [`::reqwest::Client::new`]: if the TLS backend cannot be initialised.
  pub fn from_config(config: &HttpClientConfig) -> Self {
    let ssrf = Arc::new(config.ssrf.clone());
    let client = ::reqwest::Client::builder()
      .connect_timeout(config.connect_timeout)
      .timeout(config.timeout)
      .dns_resolver(ssrf::GuardedResolver::new(Arc::clone(&ssrf)))
      .redirect(ssrf::redirect_policy(
        Arc::clone(&ssrf),
        config.max_redirects,
      ))
      .build()
      .expect("reqwest client: TLS backend cannot be initialised");
    Self {
      inner: client,
      ssrf: Some(ssrf),
    }
  }
}

/// Client-wide defaults for the reqwest layer, configured once for every call
/// ([`ReqwestHttpClientProvider::layer_with`]).
#[derive(Clone, Debug)]
pub struct HttpClientConfig {
  /// Timeout for establishing a connection.
  pub connect_timeout: Duration,
  /// Total timeout per request (a request's own [`HttpRequest::timeout`] replaces it).
  pub timeout: Duration,
  /// Retry policy applied to every call ([`RetryPolicy::none`] to disable).
  pub retry: RetryPolicy,
  /// The SSRF guard: which destinations requests may reach (on by default, [`SsrfPolicy`]).
  pub ssrf: SsrfPolicy,
  /// Redirects followed per request, each hop re-checked against [`ssrf`](Self::ssrf); 0 (the
  /// default) returns a `3xx` response as is.
  pub max_redirects: usize,
}

impl HttpClientConfig {
  /// Default connect timeout (10 s).
  pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
  /// Default total request timeout (30 s).
  pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

  /// Replaces the connect timeout.
  #[inline]
  pub fn with_connect_timeout(mut self, connect: Duration) -> Self {
    self.connect_timeout = connect;
    self
  }

  /// Replaces the total request timeout.
  #[inline]
  pub fn with_timeout(mut self, total: Duration) -> Self {
    self.timeout = total;
    self
  }

  /// Replaces the retry policy.
  #[inline]
  pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
    self.retry = retry;
    self
  }

  /// Replaces the SSRF policy (one read from config, for instance).
  #[inline]
  pub fn with_ssrf(mut self, ssrf: SsrfPolicy) -> Self {
    self.ssrf = ssrf;
    self
  }

  /// Exempts `hosts` from the SSRF range check ([`SsrfPolicy::allow_hosts`]).
  pub fn allow_hosts<I, S>(mut self, hosts: I) -> Self
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    self.ssrf = self.ssrf.allow_hosts(hosts);
    self
  }

  /// Exempts the ranges `cidrs` from the SSRF range check ([`SsrfPolicy::allow_cidrs`]).
  pub fn allow_cidrs(mut self, cidrs: impl IntoIterator<Item = IpCidr>) -> Self {
    self.ssrf = self.ssrf.allow_cidrs(cidrs);
    self
  }

  /// Follows up to `max` redirects per request, checking every hop against the SSRF policy.
  #[inline]
  pub fn follow_redirects(mut self, max: usize) -> Self {
    self.max_redirects = max;
    self
  }

  /// Turns the SSRF range check off ([`SsrfPolicy::dangerous_allow_private_networks`], recorded
  /// in the posture registry).
  pub fn dangerous_allow_private_networks(mut self) -> Self {
    self.ssrf = self.ssrf.dangerous_allow_private_networks();
    self
  }
}

impl Default for HttpClientConfig {
  /// Connect 10 s, total 30 s, [`RetryPolicy::default`], [`SsrfPolicy::default`] (guard on, no
  /// exemption), no redirects followed.
  fn default() -> Self {
    Self {
      connect_timeout: Self::DEFAULT_CONNECT_TIMEOUT,
      timeout: Self::DEFAULT_TIMEOUT,
      retry: RetryPolicy::default(),
      ssrf: SsrfPolicy::default(),
      max_redirects: 0,
    }
  }
}

/// The reqwest request for `req` (header validation happens here, before any I/O).
fn reqwest_request(
  client: &::reqwest::Client,
  req: &HttpRequest,
) -> Result<::reqwest::RequestBuilder, HttpError> {
  let method = match req.method {
    HttpMethod::Get => ::reqwest::Method::GET,
    HttpMethod::Post => ::reqwest::Method::POST,
    HttpMethod::Put => ::reqwest::Method::PUT,
    HttpMethod::Delete => ::reqwest::Method::DELETE,
    HttpMethod::Patch => ::reqwest::Method::PATCH,
  };
  let mut rb = client.request(method, &req.url);
  if let Some(t) = req.timeout {
    rb = rb.timeout(t);
  }
  for (k, v) in &req.headers {
    let name = HeaderName::from_bytes(k.as_bytes())
      .map_err(|e| HttpError::InvalidRequest(format!("bad header name {k:?}: {e}")))?;
    let value = HeaderValue::from_str(v)
      .map_err(|e| HttpError::InvalidRequest(format!("bad header value for {k:?}: {e}")))?;
    rb = rb.header(name, value);
  }
  if let Some(b) = &req.body {
    rb = rb.body(b.clone());
  }
  Ok(rb)
}

/// A failed send as an [`HttpError`]: [`HttpError::InvalidRequest`] when the SSRF policy refused
/// it (in the resolver or on a redirect hop), else [`HttpError::Reqwest`].
fn send_error(e: ::reqwest::Error) -> HttpError {
  match ssrf::blocked_by(&e) {
    Some(blocked) => HttpError::InvalidRequest(blocked.to_string()),
    None => HttpError::from(e),
  }
}

/// Sends `req` (after `ssrf`'s URL check) and returns the response with its status and readable
/// headers.
async fn send(
  client: &::reqwest::Client,
  ssrf: Option<&SsrfPolicy>,
  req: &HttpRequest,
) -> Result<(::reqwest::Response, u16, Vec<(String, String)>), HttpError> {
  let rb = reqwest_request(client, req)?;
  // An unparsable URL is left to reqwest, which fails the send with its own error.
  if let (Some(policy), Ok(url)) = (ssrf, ::reqwest::Url::parse(&req.url)) {
    policy
      .check_parsed(&url)
      .map_err(|blocked| HttpError::InvalidRequest(blocked.to_string()))?;
  }
  let resp = rb.send().await.map_err(send_error)?;
  let status = resp.status().as_u16();
  let headers = resp
    .headers()
    .iter()
    .filter_map(|(k, v)| {
      v.to_str()
        .ok()
        .map(|s| (k.as_str().to_string(), s.to_string()))
    })
    .collect();
  Ok((resp, status, headers))
}

impl HttpClient for ReqwestHttpClient {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    let client = self.inner.clone();
    let ssrf = self.ssrf.clone();
    from_async(move || async move {
      let max = req.max_body();
      let (resp, status, headers) = send(&client, ssrf.as_deref(), &req).await?;
      // The cap holds while reading: never more than `max` bytes (plus one chunk) in memory.
      let body = reqwest_source(resp, max).read_to_end().await?;
      Ok(HttpResponse {
        status,
        headers,
        body,
      })
    })
  }

  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    let client = self.inner.clone();
    let ssrf = self.ssrf.clone();
    from_async(move || async move {
      let max = req.max_body();
      let (resp, status, headers) = send(&client, ssrf.as_deref(), &req).await?;
      Ok(StreamingHttpResponse {
        status,
        headers,
        body: HttpBody::from_reqwest(resp, max),
      })
    })
  }
}

/// The [`HttpClient`] capability: a shared handle to the installed implementation.
#[derive(Clone)]
pub struct HttpClientService(pub Arc<dyn HttpClient>);

impl HttpClientService {
  /// Wrap `client` as the capability.
  #[inline]
  pub fn new(client: impl HttpClient) -> Self {
    Self(Arc::new(client))
  }
}

impl std::ops::Deref for HttpClientService {
  type Target = dyn HttpClient;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl std::fmt::Debug for HttpClientService {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("HttpClientService")
      .field(&"<dyn HttpClient>")
      .finish()
  }
}

/// The minimal typed environment holding only [`HttpClientService`] (the output of the HTTP
/// layers).
pub type HttpClientEnv = HCons<HttpClientService, HNil>;

/// An [`HttpClientEnv`] wrapping `client` (for `.provide_all(..)` in tests and entry points).
#[inline]
pub fn http_client_env(client: impl HttpClient) -> HttpClientEnv {
  hlist![HttpClientService::new(client)]
}

/// A [`Layer`] providing `client` as the [`HttpClientService`] capability (test doubles, a
/// configured [`ReqwestHttpClient`]).
#[inline]
pub fn provide_http_client<E: Send + 'static>(client: impl HttpClient) -> Layer<HttpClientEnv, E> {
  Layer::succeed(http_client_env(client))
}

/// The default reqwest-backed [`HttpClient`] provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReqwestHttpClientProvider;

impl ReqwestHttpClientProvider {
  /// The layer with [`HttpClientConfig::default`]: a [`ReqwestHttpClient`] with the default
  /// timeouts, SSRF guard and no redirects, behind a [`RetryingHttpClient`] with
  /// [`RetryPolicy::default`], built once per layer build.
  pub fn layer<E: Send + 'static>() -> Layer<HttpClientEnv, E> {
    Self::layer_with(HttpClientConfig::default())
  }

  /// The layer with `config`'s timeouts, SSRF policy, redirect limit and retry policy, built once
  /// per layer build.
  pub fn layer_with<E: Send + 'static>(config: HttpClientConfig) -> Layer<HttpClientEnv, E> {
    Layer::effect(move || {
      let config = config.clone();
      sync(move || {
        let client = ReqwestHttpClient::from_config(&config);
        hlist![HttpClientService::new(RetryingHttpClient::new(
          client,
          config.retry
        ))]
      })
    })
  }
}

/// The default reqwest-backed [`HttpClient`] layer ([`ReqwestHttpClientProvider::layer`]).
#[inline]
pub fn provide_reqwest_http_client<E: Send + 'static>() -> Layer<HttpClientEnv, E> {
  ReqwestHttpClientProvider::layer()
}

/// The reqwest-backed [`HttpClient`] layer with `config`
/// ([`ReqwestHttpClientProvider::layer_with`]).
#[inline]
pub fn provide_reqwest_http_client_with<E: Send + 'static>(
  config: HttpClientConfig,
) -> Layer<HttpClientEnv, E> {
  ReqwestHttpClientProvider::layer_with(config)
}

/// Execute using the [`HttpClientService`] in `R`.
#[inline]
pub fn execute<R, I>(req: HttpRequest) -> Effect<HttpResponse, HttpError, R>
where
  R: Selector<HttpClientService, I> + HList,
{
  need::<HttpClientService>().flat_map(move |client| client.execute(req).narrow())
}

/// Streamed execute using the [`HttpClientService`] in `R`.
#[inline]
pub fn execute_stream<R, I>(req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError, R>
where
  R: Selector<HttpClientService, I> + HList,
{
  need::<HttpClientService>().flat_map(move |client| client.execute_stream(req).narrow())
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{Cause, Exit};

  fn run_blocking<A: Send + 'static>(eff: Effect<A, HttpError>) -> Result<A, HttpError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  mod http_request {
    use super::*;

    mod get {
      use super::*;

      #[test]
      fn sets_get_method_and_empty_optional_fields() {
        let r = HttpRequest::get("https://a.test/");
        assert_eq!(r.method, HttpMethod::Get);
        assert_eq!(r.url, "https://a.test/");
        assert!(r.headers.is_empty());
        assert!(r.body.is_none());
        assert!(r.timeout.is_none());
        assert!(r.max_body_bytes.is_none());
      }
    }

    mod post {
      use super::*;

      #[test]
      fn sets_post_method_and_body() {
        let body = vec![1, 2, 3];
        let r = HttpRequest::post("https://a.test/p", body.clone());
        assert_eq!(r.method, HttpMethod::Post);
        assert_eq!(r.body, Some(body));
      }
    }

    #[test]
    fn default_max_body_bytes_constant_is_ten_mib() {
      assert_eq!(HttpRequest::DEFAULT_MAX_BODY_BYTES, 10 * 1024 * 1024);
    }
  }

  mod http_body {
    use super::*;

    #[test]
    fn from_bytes_splits_into_chunks_in_order() {
      let body = HttpBody::from_bytes(&[1, 2, 3, 4, 5], 2);
      assert_eq!(run_blocking(body.next_chunk()).unwrap(), Some(vec![1, 2]));
      assert_eq!(run_blocking(body.next_chunk()).unwrap(), Some(vec![3, 4]));
      assert_eq!(run_blocking(body.next_chunk()).unwrap(), Some(vec![5]));
      assert_eq!(run_blocking(body.next_chunk()).unwrap(), None);
      assert_eq!(run_blocking(body.next_chunk()).unwrap(), None);
    }

    #[test]
    fn collect_reads_the_rest() {
      let body = HttpBody::from_chunks([vec![1], vec![2, 3]]);
      assert_eq!(run_blocking(body.next_chunk()).unwrap(), Some(vec![1]));
      assert_eq!(run_blocking(body.collect()).unwrap(), vec![2, 3]);
    }

    #[test]
    fn empty_and_zero_chunk_size() {
      assert!(
        run_blocking(HttpBody::empty().collect())
          .unwrap()
          .is_empty()
      );
      let body = HttpBody::from_bytes(&[9, 8], 0);
      assert_eq!(run_blocking(body.next_chunk()).unwrap(), Some(vec![9]));
      assert!(format!("{body:?}").contains("HttpBody"));
    }

    #[test]
    fn response_body_stream_streams_the_buffered_body() {
      let resp = HttpResponse {
        status: 200,
        headers: vec![],
        body: vec![1, 2, 3],
      };
      let collected = run_blocking(response_body_stream(&resp, 0).collect()).unwrap();
      assert_eq!(collected, vec![1, 2, 3]);
    }
  }

  mod capability {
    use super::*;
    use id_effect::{fail, succeed};

    struct Echo;

    impl HttpClient for Echo {
      fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
        succeed(HttpResponse {
          status: 200,
          headers: vec![],
          body: req.url.into_bytes(),
        })
      }

      fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
        if req.url.is_empty() {
          return fail(HttpError::InvalidRequest("empty url".into()));
        }
        succeed(StreamingHttpResponse {
          status: 200,
          headers: vec![],
          body: HttpBody::from_bytes(req.url.as_bytes(), 3),
        })
      }
    }

    #[test]
    fn execute_and_execute_stream_use_the_provided_service() {
      let env = http_client_env(Echo);
      let res = run_blocking(execute(HttpRequest::get("e://x")).provide_all(env.clone())).unwrap();
      assert_eq!(res.body, b"e://x");
      let streamed =
        run_blocking(execute_stream(HttpRequest::get("e://stream")).provide_all(env.clone()))
          .unwrap();
      assert_eq!(
        run_blocking(streamed.body.collect()).unwrap(),
        b"e://stream"
      );
      let err = run_blocking(execute_stream(HttpRequest::get("")).provide_all(env)).unwrap_err();
      assert!(matches!(err, HttpError::InvalidRequest(_)));
    }

    #[test]
    fn provide_http_client_layer_supplies_the_capability() {
      let program = execute::<HttpClientEnv, _>(HttpRequest::get("e://layer"))
        .provide_layer(provide_http_client(Echo));
      assert_eq!(run_blocking(program).unwrap().body, b"e://layer");
    }

    #[test]
    fn reqwest_provider_layer_builds_a_fresh_client_per_build() {
      let layer = provide_reqwest_http_client::<HttpError>();
      let a = run_blocking(layer.build()).unwrap();
      let b = run_blocking(layer.build()).unwrap();
      assert!(!Arc::ptr_eq(&a.0, &b.0));
      assert!(format!("{:?}", a.0).contains("HttpClientService"));
    }
  }

  // Header validation happens before any I/O, so these never touch the network.
  mod execute_stream_invalid_headers {
    use super::*;

    fn request(method: HttpMethod, header: (&str, &str)) -> HttpRequest {
      let mut req = HttpRequest::get("http://127.0.0.1:9/");
      req.method = method;
      req.timeout = Some(std::time::Duration::from_secs(1));
      req.headers = vec![(header.0.to_string(), header.1.to_string())];
      req
    }

    #[test]
    fn bad_header_name_is_invalid_request_for_every_method() {
      let client = ReqwestHttpClient::default_client();
      for method in [
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Delete,
        HttpMethod::Patch,
      ] {
        let out = run_blocking(client.execute_stream(request(method, ("bad name", "v"))));
        assert!(
          matches!(&out, Err(HttpError::InvalidRequest(m)) if m.contains("bad header name")),
          "{:?}",
          out.err()
        );
      }
    }

    #[test]
    fn bad_header_value_is_invalid_request() {
      let client = ReqwestHttpClient::default_client();
      let out =
        run_blocking(client.execute_stream(request(HttpMethod::Get, ("x-ok", "line\nbreak"))));
      assert!(
        matches!(&out, Err(HttpError::InvalidRequest(m)) if m.contains("bad header value")),
        "{:?}",
        out.err()
      );
    }
  }
}

pub mod reqwest;
