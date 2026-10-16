//! HTTP response caching: [`HttpCacheLayer`] sets `Cache-Control` on `GET` responses, gives them
//! an `ETag` and answers conditional requests with `304 Not Modified`.
//!
//! For a `200` answer to `GET` the layer
//!
//! 1. inserts its [`CacheControl`] unless the handler already set `Cache-Control`
//!    (a `no-store` response is left alone from here on);
//! 2. keeps a handler's own `ETag`, or computes a strong one from a SHA-256 of the body when the
//!    body is at most [`body_limit`](HttpCacheLayer::body_limit) bytes (larger or streaming
//!    bodies are passed through unbuffered and get no `ETag`);
//! 3. answers `If-None-Match` (weak comparison, `*` included) with `304`, or, without
//!    `If-None-Match`, `If-Modified-Since` against the handler's `Last-Modified`.
//!
//! A `304` carries `ETag`, `Cache-Control`, `Vary`, `Last-Modified`, `Expires`, `Date` and
//! `Content-Location` and no body. `HEAD` responses get `Cache-Control` and are matched only
//! against an `ETag` the handler set (their body is empty, so there is nothing to hash). The
//! handler still runs for a conditional request: the layer saves bandwidth, not work.
//!
//! Put the layer **outside** [`CompressionLayer`](super::CompressionLayer) so the `ETag` is the
//! hash of the bytes actually sent (each encoding gets its own tag), or switch to
//! [`weak_etags`](HttpCacheLayer::weak_etags) when it sits inside.
//!
//! ```rust
//! use std::time::Duration;
//!
//! use axum::{Router, routing::get};
//! use id_effect_axum::middleware::{CacheControl, CompressionLayer, HttpCacheLayer};
//!
//! let app: Router = Router::new()
//!   .route("/catalog", get(|| async { "the whole catalog" }))
//!   .layer(CompressionLayer::new())
//!   .layer(HttpCacheLayer::new().cache_control(CacheControl::public(Duration::from_secs(60))));
//! # let _ = app;
//! ```

use std::collections::VecDeque;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body::{Body as _, Frame};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower_layer::Layer;
use tower_service::Service;

/// [`HttpCacheLayer::default`]: bodies up to 1 MiB are hashed for an `ETag`.
pub const DEFAULT_ETAG_BODY_LIMIT: usize = 1024 * 1024;

/// One year, the `max-age` of [`CacheControl::immutable`].
const ONE_YEAR: u64 = 365 * 24 * 60 * 60;

/// A `Cache-Control` response header value, built from the common presets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheControl(HeaderValue);

impl CacheControl {
  /// `no-cache`: any cache may store the response but must revalidate it (`ETag` /
  /// `Last-Modified`) before every reuse. The right policy for HTML entry points.
  pub fn no_cache() -> Self {
    Self(HeaderValue::from_static("no-cache"))
  }

  /// `private, no-cache`: only the browser may store it, and must revalidate before reuse
  /// ([`HttpCacheLayer`]'s default: safe for per-user responses).
  pub fn private_no_cache() -> Self {
    Self(HeaderValue::from_static("private, no-cache"))
  }

  /// `no-store`: never stored by any cache.
  pub fn no_store() -> Self {
    Self(HeaderValue::from_static("no-store"))
  }

  /// `public, max-age=31536000, immutable`: for content-hashed assets (`app.3f2a9c1b.js`)
  /// whose URL changes whenever their content does.
  pub fn immutable() -> Self {
    Self::header(format!("public, max-age={ONE_YEAR}, immutable"))
  }

  /// `public, max-age=<seconds>`: shared caches may reuse it for `max_age` without asking.
  pub fn public(max_age: Duration) -> Self {
    Self::header(format!("public, max-age={}", max_age.as_secs()))
  }

  /// `private, max-age=<seconds>`: only the browser may reuse it, for `max_age`.
  pub fn private(max_age: Duration) -> Self {
    Self::header(format!("private, max-age={}", max_age.as_secs()))
  }

  /// Any other directive list, e.g. `"public, max-age=60, stale-while-revalidate=30"`.
  ///
  /// # Panics
  ///
  /// When `value` is not a valid header value (control characters).
  pub fn from_static(value: &'static str) -> Self {
    Self(HeaderValue::from_static(value))
  }

  /// The header value.
  pub fn header_value(&self) -> &HeaderValue {
    &self.0
  }

  fn header(value: String) -> Self {
    // Digits, letters, commas and spaces only.
    Self(HeaderValue::try_from(value).expect("a valid Cache-Control value"))
  }
}

/// Sets `Cache-Control` and an `ETag` on `GET` responses and answers `If-None-Match` /
/// `If-Modified-Since` with `304` (see the [module docs](self)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpCacheLayer {
  cache_control: CacheControl,
  body_limit: usize,
  weak: bool,
}

impl Default for HttpCacheLayer {
  fn default() -> Self {
    Self::new()
  }
}

impl HttpCacheLayer {
  /// `Cache-Control: private, no-cache`, strong `ETag`s for bodies up to
  /// [`DEFAULT_ETAG_BODY_LIMIT`].
  pub fn new() -> Self {
    Self {
      cache_control: CacheControl::private_no_cache(),
      body_limit: DEFAULT_ETAG_BODY_LIMIT,
      weak: false,
    }
  }

  /// The `Cache-Control` set on responses that do not carry their own.
  pub fn cache_control(mut self, cache_control: CacheControl) -> Self {
    self.cache_control = cache_control;
    self
  }

  /// Hash bodies of at most `limit` bytes; larger ones stream through without an `ETag`.
  pub fn body_limit(mut self, limit: usize) -> Self {
    self.body_limit = limit;
    self
  }

  /// Emit weak `ETag`s (`W/"…"`), for a layer placed inside a compression layer.
  pub fn weak_etags(mut self) -> Self {
    self.weak = true;
    self
  }
}

impl<S> Layer<S> for HttpCacheLayer {
  type Service = HttpCacheService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    HttpCacheService {
      inner,
      config: self.clone(),
    }
  }
}

/// The [`Service`] produced by [`HttpCacheLayer`].
#[derive(Clone, Debug)]
pub struct HttpCacheService<S> {
  inner: S,
  config: HttpCacheLayer,
}

impl<S> Service<Request> for HttpCacheService<S>
where
  S: Service<Request, Error = Infallible>,
  S::Response: IntoResponse,
  S::Future: Send + 'static,
{
  type Response = Response;
  type Error = Infallible;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, request: Request) -> Self::Future {
    let method = request.method().clone();
    let if_none_match = request.headers().get(header::IF_NONE_MATCH).cloned();
    let if_modified_since = request.headers().get(header::IF_MODIFIED_SINCE).cloned();
    let config = self.config.clone();
    let call = self.inner.call(request);
    Box::pin(async move {
      let response = match call.await {
        Ok(response) => response.into_response(),
        Err(never) => match never {},
      };
      if (method != Method::GET && method != Method::HEAD) || response.status() != StatusCode::OK {
        return Ok(response);
      }
      Ok(
        config
          .apply(
            method == Method::GET,
            if_none_match,
            if_modified_since,
            response,
          )
          .await,
      )
    })
  }
}

impl HttpCacheLayer {
  async fn apply(
    self,
    hash_body: bool,
    if_none_match: Option<HeaderValue>,
    if_modified_since: Option<HeaderValue>,
    response: Response,
  ) -> Response {
    let (mut parts, body) = response.into_parts();
    let cache_control = parts
      .headers
      .entry(header::CACHE_CONTROL)
      .or_insert(self.cache_control.0);
    if has_directive(cache_control, "no-store") {
      return Response::from_parts(parts, body);
    }
    let (etag, body) = match parts.headers.get(header::ETAG) {
      Some(etag) => (Some(etag.clone()), body),
      None if hash_body => match buffer(body, self.body_limit).await {
        Ok(bytes) => {
          let etag = body_etag(&bytes, self.weak);
          parts.headers.insert(header::ETAG, etag.clone());
          (Some(etag), Body::from(bytes))
        }
        Err(body) => (None, body),
      },
      None => (None, body),
    };
    let not_modified = match (&if_none_match, &etag) {
      (Some(condition), Some(etag)) => weak_match(condition, etag),
      (Some(_), None) => false,
      (None, _) => if_modified_since
        .as_ref()
        .is_some_and(|since| unmodified_since(&parts.headers, since)),
    };
    if not_modified {
      return not_modified_response(&parts.headers);
    }
    Response::from_parts(parts, body)
  }
}

/// Whether the `Cache-Control` value lists `directive` (case-insensitive).
fn has_directive(value: &HeaderValue, directive: &str) -> bool {
  value.to_str().is_ok_and(|v| {
    v.split(',').any(|d| {
      d.split('=')
        .next()
        .is_some_and(|name| name.trim().eq_ignore_ascii_case(directive))
    })
  })
}

/// Collect `body` when it holds at most `limit` bytes of data and no trailers; otherwise a body
/// that replays what was read and then streams the rest.
async fn buffer(mut body: Body, limit: usize) -> Result<Bytes, Body> {
  if body.size_hint().lower() > limit as u64 {
    return Err(body);
  }
  let mut frames = VecDeque::new();
  let mut size = 0usize;
  loop {
    match body.frame().await {
      None => break,
      Some(Ok(frame)) => {
        let over = match frame.data_ref() {
          Some(data) => {
            size += data.len();
            size > limit
          }
          None => true,
        };
        frames.push_back(frame);
        if over {
          return Err(Body::new(Replay {
            frames,
            error: None,
            rest: body,
          }));
        }
      }
      Some(Err(error)) => {
        return Err(Body::new(Replay {
          frames,
          error: Some(error),
          rest: Body::empty(),
        }));
      }
    }
  }
  let mut bytes = Vec::with_capacity(size);
  for frame in frames {
    if let Ok(data) = frame.into_data() {
      bytes.extend_from_slice(&data);
    }
  }
  Ok(Bytes::from(bytes))
}

/// The frames already read from a body, then its error or the remainder.
struct Replay {
  frames: VecDeque<Frame<Bytes>>,
  error: Option<axum::Error>,
  rest: Body,
}

impl http_body::Body for Replay {
  type Data = Bytes;
  type Error = axum::Error;

  fn poll_frame(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
    let this = self.get_mut();
    if let Some(frame) = this.frames.pop_front() {
      return Poll::Ready(Some(Ok(frame)));
    }
    if let Some(error) = this.error.take() {
      return Poll::Ready(Some(Err(error)));
    }
    Pin::new(&mut this.rest).poll_frame(cx)
  }

  fn is_end_stream(&self) -> bool {
    self.frames.is_empty() && self.error.is_none() && self.rest.is_end_stream()
  }
}

/// `"<32 hex digits of SHA-256>"`, or its weak form.
fn body_etag(body: &[u8], weak: bool) -> HeaderValue {
  let digest = Sha256::digest(body);
  let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
  let tag = if weak {
    format!("W/\"{hex}\"")
  } else {
    format!("\"{hex}\"")
  };
  HeaderValue::try_from(tag).expect("hex digits form a valid ETag")
}

/// One entity tag of an `ETag` value or an `If-None-Match` / `If-Match` list.
#[derive(Debug, PartialEq, Eq)]
struct EntityTag<'a> {
  weak: bool,
  opaque: &'a str,
}

/// The entity tags of a header list (`*` → `Err(())`); parsing stops at the first malformed tag.
fn entity_tags(value: &str) -> Result<Vec<EntityTag<'_>>, ()> {
  let mut tags = Vec::new();
  let mut rest = value;
  loop {
    rest = rest.trim_start_matches([' ', '\t', ',']);
    if rest.is_empty() {
      return Ok(tags);
    }
    if rest.starts_with('*') {
      return Err(());
    }
    let (weak, tail) = match rest.strip_prefix("W/") {
      Some(tail) => (true, tail),
      None => (false, rest),
    };
    let Some(tail) = tail.strip_prefix('"') else {
      return Ok(tags);
    };
    let Some(end) = tail.find('"') else {
      return Ok(tags);
    };
    tags.push(EntityTag {
      weak,
      opaque: &tail[..end],
    });
    rest = &tail[end + 1..];
  }
}

/// The single entity tag in `etag`.
fn single_tag(etag: &HeaderValue) -> Option<EntityTag<'_>> {
  entity_tags(etag.to_str().ok()?).ok()?.into_iter().next()
}

/// RFC 9110 weak comparison of `condition` (an `If-None-Match` list) against `etag`.
pub(crate) fn weak_match(condition: &HeaderValue, etag: &HeaderValue) -> bool {
  let Some(etag) = single_tag(etag) else {
    return false;
  };
  match condition.to_str().map(entity_tags) {
    Ok(Err(())) => true,
    Ok(Ok(tags)) => tags.iter().any(|tag| tag.opaque == etag.opaque),
    Err(_) => false,
  }
}

/// RFC 9110 strong comparison of `condition` (an `If-Match` list or an `If-Range` tag) against
/// `etag`.
pub(crate) fn strong_match(condition: &HeaderValue, etag: &HeaderValue) -> bool {
  let Some(etag) = single_tag(etag) else {
    return false;
  };
  match condition.to_str().map(entity_tags) {
    Ok(Err(())) => true,
    Ok(Ok(tags)) => {
      !etag.weak
        && tags
          .iter()
          .any(|tag| !tag.weak && tag.opaque == etag.opaque)
    }
    Err(_) => false,
  }
}

/// Whether the response's `Last-Modified` is not later than `since` (an `If-Modified-Since`).
fn unmodified_since(headers: &HeaderMap, since: &HeaderValue) -> bool {
  let parse = |v: &HeaderValue| httpdate::parse_http_date(v.to_str().ok()?).ok();
  match (
    headers.get(header::LAST_MODIFIED).and_then(parse),
    parse(since),
  ) {
    (Some(modified), Some(since)) => modified <= since,
    _ => false,
  }
}

/// A `304 Not Modified` carrying the validator and caching headers of the full response.
pub(crate) fn not_modified_response(headers: &HeaderMap) -> Response {
  let mut response = StatusCode::NOT_MODIFIED.into_response();
  for name in [
    header::CACHE_CONTROL,
    header::CONTENT_LOCATION,
    header::DATE,
    header::ETAG,
    header::EXPIRES,
    header::LAST_MODIFIED,
    header::VARY,
  ] {
    for value in headers.get_all(&name) {
      response.headers_mut().append(name.clone(), value.clone());
    }
  }
  response
}

#[cfg(test)]
mod tests {
  use super::*;

  fn hv(s: &'static str) -> HeaderValue {
    HeaderValue::from_static(s)
  }

  #[test]
  fn entity_tag_lists_parse() {
    assert_eq!(
      entity_tags(r#" "a", W/"b",,"c""#).unwrap(),
      vec![
        EntityTag {
          weak: false,
          opaque: "a"
        },
        EntityTag {
          weak: true,
          opaque: "b"
        },
        EntityTag {
          weak: false,
          opaque: "c"
        },
      ]
    );
    assert!(entity_tags("*").is_err());
    assert_eq!(entity_tags("bare").unwrap(), vec![]);
    assert_eq!(entity_tags(r#""a", "unterminated"#).unwrap().len(), 1);
  }

  #[test]
  fn weak_and_strong_comparison() {
    assert!(weak_match(&hv(r#"W/"x""#), &hv(r#""x""#)));
    assert!(weak_match(&hv("*"), &hv(r#""x""#)));
    assert!(!weak_match(&hv(r#""y""#), &hv(r#""x""#)));
    assert!(!weak_match(&hv(r#""x""#), &hv("not-a-tag")));
    assert!(!weak_match(
      &HeaderValue::from_bytes(b"\"x\xff\"").unwrap(),
      &hv(r#""x""#)
    ));
    assert!(strong_match(&hv(r#""x""#), &hv(r#""x""#)));
    assert!(strong_match(&hv("*"), &hv(r#""x""#)));
    assert!(!strong_match(&hv(r#"W/"x""#), &hv(r#""x""#)));
    assert!(!strong_match(&hv(r#""x""#), &hv(r#"W/"x""#)));
    assert!(!strong_match(&hv(r#""x""#), &hv("")));
    assert!(!strong_match(
      &HeaderValue::from_bytes(b"\"x\xff\"").unwrap(),
      &hv(r#""x""#)
    ));
  }

  #[test]
  fn cache_control_presets() {
    assert_eq!(CacheControl::no_cache().header_value(), "no-cache");
    assert_eq!(CacheControl::no_store().header_value(), "no-store");
    assert_eq!(
      CacheControl::immutable().header_value(),
      "public, max-age=31536000, immutable"
    );
    assert_eq!(
      CacheControl::private(Duration::from_secs(5)).header_value(),
      "private, max-age=5"
    );
    assert_eq!(
      CacheControl::from_static("max-age=1, stale-while-revalidate=9").header_value(),
      "max-age=1, stale-while-revalidate=9"
    );
    assert!(has_directive(&hv("private, No-Store"), "no-store"));
    assert!(!has_directive(&hv("max-age=1"), "no-store"));
    assert!(!has_directive(
      &HeaderValue::from_bytes(b"\xff").unwrap(),
      "no-store"
    ));
    assert_eq!(HttpCacheLayer::default(), HttpCacheLayer::new());
  }

  #[tokio::test]
  async fn a_failing_body_is_replayed_with_its_error() {
    let stream = futures_stream(vec![
      Ok(Bytes::from_static(b"ab")),
      Err(std::io::Error::other("boom")),
    ]);
    let body = buffer(stream, 1024).await.expect_err("not buffered");
    assert!(!body.is_end_stream());
    let mut body = body;
    let first = body.frame().await.unwrap().unwrap();
    assert_eq!(first.into_data().unwrap(), "ab");
    assert!(body.frame().await.unwrap().is_err());
  }

  #[tokio::test]
  async fn a_body_over_the_limit_streams_through_unchanged() {
    let stream = futures_stream(vec![
      Ok(Bytes::from_static(b"abc")),
      Ok(Bytes::from_static(b"def")),
      Ok(Bytes::from_static(b"ghi")),
    ]);
    let body = buffer(stream, 4).await.expect_err("over the limit");
    assert_eq!(body.collect().await.unwrap().to_bytes(), "abcdefghi");
    let small = futures_stream(vec![Ok(Bytes::from_static(b"ab"))]);
    assert_eq!(buffer(small, 4).await.unwrap(), "ab");
  }

  #[tokio::test]
  async fn trailers_stop_buffering() {
    let mut trailers = HeaderMap::new();
    trailers.insert("x-checksum", hv("1"));
    let body = Body::new(
      http_body_util::Full::new(Bytes::from_static(b"data"))
        .map_err(|never| match never {})
        .with_trailers(async move { Some(Ok(trailers)) }),
    );
    let body = buffer(body, 1024)
      .await
      .expect_err("trailers are not buffered");
    let collected = body.collect().await.unwrap();
    assert!(collected.trailers().is_some());
    assert_eq!(collected.to_bytes(), "data");
  }

  /// A body yielding `chunks` with no size hint.
  fn futures_stream(chunks: Vec<Result<Bytes, std::io::Error>>) -> Body {
    let frames = chunks
      .into_iter()
      .map(|chunk| chunk.map(Frame::data))
      .collect::<VecDeque<_>>();
    Body::new(Chunks(frames))
  }

  struct Chunks(VecDeque<Result<Frame<Bytes>, std::io::Error>>);

  impl http_body::Body for Chunks {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
      self: Pin<&mut Self>,
      _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
      Poll::Ready(self.get_mut().0.pop_front())
    }
  }
}
