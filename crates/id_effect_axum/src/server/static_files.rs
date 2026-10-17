//! Static files: [`serve_dir`] serves a directory with conditional GET, byte ranges, precompressed
//! variants and a `Cache-Control` policy, on top of tower-http's `ServeDir`.
//!
//! - **Validators.** Every file response carries `Last-Modified` and a strong `ETag` derived from
//!   the modification time, the size and the content encoding (`"<mtime>-<size>[-br|-gzip]"`).
//!   `If-None-Match` (weak comparison) and `If-Modified-Since` answer `304 Not Modified`;
//!   `If-None-Match` takes precedence, as RFC 9110 requires. `If-Match` / `If-Unmodified-Since`
//!   answer `412`, and `If-Range` (entity tag or date) decides whether a `Range` is honoured.
//! - **Content negotiation.** With precompression on (the default) a request accepting `br` or
//!   `gzip` gets `style.css.br` / `style.css.gz` when that file exists next to `style.css`, with
//!   `Content-Encoding` set; every response says `Vary: Accept-Encoding`.
//! - **Cache-Control.** By default content-hashed asset names (`app.3f2a9c1b.js`,
//!   `index-B2xP7k9a.css`: a `.`- or `-`-separated part of 8 to 64 letters, digits or `_` with
//!   at least one letter and one digit) get [`CacheControl::immutable`], everything else
//!   (HTML, `index.html` behind `/`, unhashed assets) [`CacheControl::no_cache`], so it is
//!   revalidated cheaply with the `ETag`. [`StaticFiles::cache_control`] fixes one policy for all.
//! - **Safe paths.** `..`, absolute and drive-prefixed paths never leave the root, and hidden
//!   files (any path segment starting with `.`, percent-encoded or not, except `.well-known`)
//!   are not served. Misses are a `404` [`Problem`](crate::Problem), other methods a `405`.
//!
//! `StaticFiles` is a tower `Service`; mount it with `Router::nest_service` or as the router's
//! `fallback_service`:
//!
//! ```rust,no_run
//! use axum::{Router, routing::get};
//! use id_effect_axum::server::serve_dir;
//!
//! let app: Router = Router::new()
//!   .route("/api/health", get(|| async { "ok" }))
//!   .nest_service("/assets", serve_dir("dist/assets"))
//!   .fallback_service(serve_dir("dist"));
//! # let _ = app;
//! ```

use std::convert::Infallible;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::UNIX_EPOCH;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use tower_http::services::ServeDir;
use tower_service::Service;

use crate::middleware::CacheControl;
use crate::middleware::http_cache::{not_modified_response, strong_match, weak_match};
use crate::middleware::problem_response;

/// Serve the files under `root` (see the [module docs](self)).
pub fn serve_dir(root: impl AsRef<Path>) -> StaticFiles {
  StaticFiles::new(root)
}

/// The static file [`Service`] built by [`serve_dir`].
#[derive(Clone, Debug)]
pub struct StaticFiles {
  root: PathBuf,
  dir: ServeDir,
  precompressed: bool,
  cache_control: Option<CacheControl>,
}

impl StaticFiles {
  /// Serve `root` with precompressed variants and the default `Cache-Control` policy.
  pub fn new(root: impl AsRef<Path>) -> Self {
    let root = root.as_ref().to_path_buf();
    Self {
      dir: Self::serve(&root, true),
      root,
      precompressed: true,
      cache_control: None,
    }
  }

  /// Look for `<file>.br` / `<file>.gz` next to a requested file (`true`, the default).
  pub fn precompressed(mut self, enabled: bool) -> Self {
    self.precompressed = enabled;
    self.dir = Self::serve(&self.root, enabled);
    self
  }

  /// Send `cache_control` with every file instead of the hashed-asset / revalidate default.
  pub fn cache_control(mut self, cache_control: CacheControl) -> Self {
    self.cache_control = Some(cache_control);
    self
  }

  fn serve(root: &Path, precompressed: bool) -> ServeDir {
    let dir = ServeDir::new(root);
    if precompressed {
      dir.precompressed_br().precompressed_gzip()
    } else {
      dir
    }
  }

  async fn respond(mut self, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    if hidden(&path) {
      return not_found(&path);
    }
    let (mut parts, _body) = request.into_parts();
    // `ServeDir` knows only the date conditions: evaluate the entity-tag ones here, and drop the
    // date condition an entity-tag condition overrides (RFC 9110 §13.2.2).
    let if_none_match = parts.headers.remove(header::IF_NONE_MATCH);
    let if_match = parts.headers.remove(header::IF_MATCH);
    if if_none_match.is_some() {
      parts.headers.remove(header::IF_MODIFIED_SINCE);
    }
    if if_match.is_some() {
      parts.headers.remove(header::IF_UNMODIFIED_SINCE);
    }
    if let Some(if_range) = parts.headers.remove(header::IF_RANGE)
      && parts.headers.contains_key(header::RANGE)
    {
      let mut probe = Request::new(Body::empty());
      *probe.method_mut() = Method::HEAD;
      *probe.uri_mut() = parts.uri.clone();
      if let Some(accept) = parts.headers.get(header::ACCEPT_ENCODING) {
        probe
          .headers_mut()
          .insert(header::ACCEPT_ENCODING, accept.clone());
      }
      let unchanged = match self.dir.try_call(probe).await {
        Ok(head) if head.status() == StatusCode::OK => if_range_holds(&if_range, head.headers()),
        _ => false,
      };
      if !unchanged {
        parts.headers.remove(header::RANGE);
      }
    }
    match self
      .dir
      .try_call(Request::from_parts(parts, Body::empty()))
      .await
    {
      Ok(response) => self.finish(response.map(Body::new), &path, if_none_match, if_match),
      Err(error) => problem_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("the file could not be read: {}", error.kind()),
        &path,
      ),
    }
  }

  fn finish(
    &self,
    mut response: Response,
    path: &str,
    if_none_match: Option<HeaderValue>,
    if_match: Option<HeaderValue>,
  ) -> Response {
    match response.status() {
      StatusCode::NOT_FOUND => not_found(path),
      StatusCode::METHOD_NOT_ALLOWED => {
        let mut problem = problem_response(
          StatusCode::METHOD_NOT_ALLOWED,
          "static files answer GET and HEAD only".to_owned(),
          path,
        );
        problem
          .headers_mut()
          .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        problem
      }
      StatusCode::PRECONDITION_FAILED => precondition_failed(path),
      StatusCode::OK | StatusCode::PARTIAL_CONTENT => {
        self.cache_headers(path, response.headers_mut());
        let etag = file_etag(response.headers());
        if let Some(etag) = &etag {
          response.headers_mut().insert(header::ETAG, etag.clone());
        }
        if let Some(if_match) = if_match
          && !etag
            .as_ref()
            .is_some_and(|etag| strong_match(&if_match, etag))
        {
          return precondition_failed(path);
        }
        if let Some(if_none_match) = if_none_match
          && etag
            .as_ref()
            .is_some_and(|etag| weak_match(&if_none_match, etag))
        {
          return not_modified_response(response.headers());
        }
        response
      }
      StatusCode::NOT_MODIFIED => {
        self.cache_headers(path, response.headers_mut());
        response
      }
      _ => response,
    }
  }

  fn cache_headers(&self, path: &str, headers: &mut HeaderMap) {
    let cache_control = match &self.cache_control {
      Some(fixed) => fixed.clone(),
      None if is_hashed_asset(path.rsplit('/').next().unwrap_or_default()) => {
        CacheControl::immutable()
      }
      None => CacheControl::no_cache(),
    };
    headers.insert(header::CACHE_CONTROL, cache_control.header_value().clone());
    if self.precompressed {
      headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
  }
}

impl Service<Request> for StaticFiles {
  type Response = Response;
  type Error = Infallible;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

  fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, request: Request) -> Self::Future {
    let files = self.clone();
    Box::pin(async move { Ok(files.respond(request).await) })
  }
}

fn not_found(path: &str) -> Response {
  problem_response(
    StatusCode::NOT_FOUND,
    "no file at this path".to_owned(),
    path,
  )
}

fn precondition_failed(path: &str) -> Response {
  problem_response(
    StatusCode::PRECONDITION_FAILED,
    "the file does not match the request's preconditions".to_owned(),
    path,
  )
}

/// `"<mtime secs>-<size>[-<encoding>]"` (hex), from the headers of a `200` / `206` file response.
fn file_etag(headers: &HeaderMap) -> Option<HeaderValue> {
  let modified = headers.get(header::LAST_MODIFIED)?.to_str().ok()?;
  let modified = httpdate::parse_http_date(modified).ok()?;
  let secs = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
  let size: u64 = match headers.get(header::CONTENT_RANGE) {
    Some(range) => range.to_str().ok()?.rsplit('/').next()?.parse().ok()?,
    None => headers
      .get(header::CONTENT_LENGTH)?
      .to_str()
      .ok()?
      .parse()
      .ok()?,
  };
  let encoding = headers
    .get(header::CONTENT_ENCODING)
    .and_then(|e| e.to_str().ok())
    .map(|e| format!("-{e}"))
    .unwrap_or_default();
  HeaderValue::try_from(format!("\"{secs:x}-{size:x}{encoding}\"")).ok()
}

/// Whether an `If-Range` validator still matches the file whose `HEAD` headers are `headers`:
/// an entity tag by strong comparison, a date by exact match with `Last-Modified`.
fn if_range_holds(if_range: &HeaderValue, headers: &HeaderMap) -> bool {
  let bytes = if_range.as_bytes();
  if bytes.starts_with(b"\"") || bytes.starts_with(b"W/") {
    file_etag(headers).is_some_and(|etag| strong_match(if_range, &etag))
  } else {
    headers.get(header::LAST_MODIFIED) == Some(if_range)
  }
}

/// Whether a (percent-encoded) request path names a hidden file or directory.
fn hidden(path: &str) -> bool {
  percent_decode(path)
    .split(|&b| b == b'/' || b == b'\\')
    .any(|segment| segment.first() == Some(&b'.') && segment != b".well-known")
}

fn percent_decode(path: &str) -> Vec<u8> {
  let bytes = path.as_bytes();
  let hex = |i: usize| bytes.get(i).and_then(|&b| (b as char).to_digit(16));
  let mut decoded = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%'
      && let (Some(high), Some(low)) = (hex(i + 1), hex(i + 2))
    {
      decoded.push((high * 16 + low) as u8);
      i += 3;
    } else {
      decoded.push(bytes[i]);
      i += 1;
    }
  }
  decoded
}

/// Whether a file name carries a content hash (see the [module docs](self)).
fn is_hashed_asset(name: &str) -> bool {
  let Some((stem, ext)) = name.rsplit_once('.') else {
    return false;
  };
  if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
    return false;
  }
  stem.split(['.', '-']).skip(1).any(|part| {
    (8..=64).contains(&part.len())
      && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
      && part.bytes().any(|b| b.is_ascii_digit())
      && part.bytes().any(|b| b.is_ascii_alphabetic())
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn hashed_asset_names() {
    assert!(is_hashed_asset("app.3f2a9c1b.js"));
    assert!(is_hashed_asset("index-B2xP7k9a.css"));
    assert!(is_hashed_asset("chunk.a1b2c3d4e5.min.js"));
    assert!(!is_hashed_asset("app.js"));
    assert!(!is_hashed_asset("3f2a9c1b.js"));
    assert!(!is_hashed_asset("jquery-3.7.1.min.js"));
    assert!(!is_hashed_asset("my-component-library.js"));
    assert!(!is_hashed_asset("report-20240101.csv"));
    assert!(!is_hashed_asset("page.3f2a9c1b.html"));
    assert!(!is_hashed_asset("LICENSE"));
    assert!(!is_hashed_asset(""));
  }

  #[test]
  fn hidden_paths() {
    assert!(hidden("/.env"));
    assert!(hidden("/a/.git/config"));
    assert!(hidden("/%2eenv"));
    assert!(hidden("/%2Egit/HEAD"));
    assert!(hidden("/a\\.ssh"));
    assert!(hidden("/../secret"));
    assert!(!hidden("/.well-known/security.txt"));
    assert!(!hidden("/app.js"));
    assert!(!hidden("/100%"));
    assert!(!hidden("/a%zz.txt"));
  }

  #[test]
  fn file_etags_need_a_date_and_a_size() {
    let mut headers = HeaderMap::new();
    assert_eq!(file_etag(&headers), None);
    headers.insert(
      header::LAST_MODIFIED,
      HeaderValue::from_static("Thu, 01 Jan 1970 00:01:00 GMT"),
    );
    assert_eq!(file_etag(&headers), None);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("255"));
    assert_eq!(file_etag(&headers).unwrap(), "\"3c-ff\"");
    headers.insert(
      header::CONTENT_RANGE,
      HeaderValue::from_static("bytes 0-9/4096"),
    );
    headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("br"));
    assert_eq!(file_etag(&headers).unwrap(), "\"3c-1000-br\"");
  }
}
