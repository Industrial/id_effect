//! Integration tests of static file serving (`server::serve_dir`, web.server#7) and HTTP response
//! caching (`middleware::HttpCacheLayer`, data.cache#7), driving a real `axum::Router` in process.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http_body_util::BodyExt;
use id_effect_axum::PROBLEM_JSON;
use id_effect_axum::middleware::{CacheControl, CompressionLayer, HttpCacheLayer};
use id_effect_axum::server::serve_dir;
use serde_json::Value;
use tower::ServiceExt;

async fn send(app: &Router, req: Request<Body>) -> Response {
  app.clone().oneshot(req).await.unwrap()
}

async fn body_bytes(res: Response) -> Bytes {
  res.into_body().collect().await.unwrap().to_bytes()
}

async fn problem(res: Response, status: StatusCode) -> Value {
  assert_eq!(res.status(), status);
  assert_eq!(res.headers()[header::CONTENT_TYPE], PROBLEM_JSON);
  let body: Value = serde_json::from_slice(&body_bytes(res).await).unwrap();
  assert_eq!(body["status"], status.as_u16());
  body
}

fn get_req(uri: &str) -> Request<Body> {
  Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn req_with(uri: &str, headers: &[(header::HeaderName, &str)]) -> Request<Body> {
  let mut builder = Request::builder().uri(uri);
  for (name, value) in headers {
    builder = builder.header(name, *value);
  }
  builder.body(Body::empty()).unwrap()
}

fn header_str(res: &Response, name: header::HeaderName) -> &str {
  res.headers()[name].to_str().unwrap()
}

/// A fresh directory with `files` (name, contents) for one test.
fn site(test: &str, files: &[(&str, &[u8])]) -> PathBuf {
  let root = std::env::temp_dir().join(format!(
    "id_effect_axum_static_{}_{test}",
    std::process::id()
  ));
  let _ = std::fs::remove_dir_all(&root);
  for (name, contents) in files {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
  }
  root
}

fn static_app(root: &PathBuf) -> Router {
  Router::new().fallback_service(serve_dir(root))
}

// ── static files (web.server#7) ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn conditional_get_returns_304() {
  let root = site(
    "conditional",
    &[
      ("index.html", b"<h1>home</h1>"),
      ("app.js", b"console.log(1)"),
    ],
  );
  let app = static_app(&root);

  let res = send(&app, get_req("/app.js")).await;
  assert_eq!(res.status(), StatusCode::OK);
  let etag = header_str(&res, header::ETAG).to_owned();
  let modified = header_str(&res, header::LAST_MODIFIED).to_owned();
  assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
  assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
  assert_eq!(res.headers()[header::VARY], "accept-encoding");
  assert_eq!(body_bytes(res).await, "console.log(1)");

  // If-None-Match: weak comparison, lists and `*`.
  for condition in [
    etag.clone(),
    format!("W/{etag}"),
    format!("\"other\", {etag}"),
    "*".to_owned(),
  ] {
    let res = send(
      &app,
      req_with("/app.js", &[(header::IF_NONE_MATCH, &condition)]),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED, "{condition}");
    assert_eq!(header_str(&res, header::ETAG), etag);
    assert_eq!(header_str(&res, header::LAST_MODIFIED), modified);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
    assert!(res.headers().get(header::CONTENT_LENGTH).is_none());
    assert!(body_bytes(res).await.is_empty());
  }

  // If-Modified-Since alone.
  let res = send(
    &app,
    req_with("/app.js", &[(header::IF_MODIFIED_SINCE, &modified)]),
  )
  .await;
  assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
  assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");

  // If-None-Match wins over If-Modified-Since (RFC 9110 §13.2.2).
  let res = send(
    &app,
    req_with(
      "/app.js",
      &[
        (header::IF_NONE_MATCH, "\"stale\""),
        (header::IF_MODIFIED_SINCE, &modified),
      ],
    ),
  )
  .await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(body_bytes(res).await, "console.log(1)");

  // HEAD revalidates too.
  let head = Request::builder()
    .method("HEAD")
    .uri("/app.js")
    .header(header::IF_NONE_MATCH, &etag)
    .body(Body::empty())
    .unwrap();
  assert_eq!(send(&app, head).await.status(), StatusCode::NOT_MODIFIED);

  // The directory index is served (and revalidated) like any HTML file.
  let res = send(&app, get_req("/")).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
  assert_eq!(body_bytes(res).await, "<h1>home</h1>");
}

#[tokio::test]
async fn etag_changes_with_body() {
  // Static: a rewritten file gets a new ETag and the old one no longer validates.
  let root = site("etag_changes", &[("data.txt", b"v1")]);
  let app = static_app(&root);
  let first = send(&app, get_req("/data.txt")).await;
  let old = header_str(&first, header::ETAG).to_owned();
  std::fs::write(root.join("data.txt"), b"version two").unwrap();
  let second = send(
    &app,
    req_with("/data.txt", &[(header::IF_NONE_MATCH, &old)]),
  )
  .await;
  assert_eq!(second.status(), StatusCode::OK);
  assert_ne!(header_str(&second, header::ETAG), old);
  assert_eq!(body_bytes(second).await, "version two");

  // Dynamic: HttpCacheLayer hashes the body.
  let hits = Arc::new(AtomicUsize::new(0));
  let counter = hits.clone();
  let app = Router::new()
    .route(
      "/count",
      get(move || {
        let counter = counter.clone();
        async move { format!("count {}", counter.load(Ordering::SeqCst)) }
      }),
    )
    .layer(HttpCacheLayer::new());
  let res = send(&app, get_req("/count")).await;
  assert_eq!(res.headers()[header::CACHE_CONTROL], "private, no-cache");
  let etag = header_str(&res, header::ETAG).to_owned();
  assert!(!etag.starts_with("W/"));
  assert_eq!(etag.len(), 34, "{etag}");
  assert_eq!(body_bytes(res).await, "count 0");

  let same = send(&app, get_req("/count")).await;
  assert_eq!(header_str(&same, header::ETAG), etag);
  let unchanged = send(&app, req_with("/count", &[(header::IF_NONE_MATCH, &etag)])).await;
  assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
  assert_eq!(header_str(&unchanged, header::ETAG), etag);
  assert_eq!(
    unchanged.headers()[header::CACHE_CONTROL],
    "private, no-cache"
  );
  assert!(body_bytes(unchanged).await.is_empty());

  hits.store(1, Ordering::SeqCst);
  let changed = send(&app, req_with("/count", &[(header::IF_NONE_MATCH, &etag)])).await;
  assert_eq!(changed.status(), StatusCode::OK);
  assert_ne!(header_str(&changed, header::ETAG), etag);
  assert_eq!(body_bytes(changed).await, "count 1");
}

#[tokio::test]
async fn precompressed_variants_are_negotiated() {
  let root = site(
    "precompressed",
    &[
      ("app.3f2a9c1b.js", b"plain javascript"),
      ("app.3f2a9c1b.js.gz", b"gzip bytes"),
      ("app.3f2a9c1b.js.br", b"brotli bytes"),
    ],
  );
  let app = static_app(&root);

  let plain = send(&app, get_req("/app.3f2a9c1b.js")).await;
  assert!(plain.headers().get(header::CONTENT_ENCODING).is_none());
  assert_eq!(
    plain.headers()[header::CACHE_CONTROL],
    "public, max-age=31536000, immutable"
  );
  let plain_tag = header_str(&plain, header::ETAG).to_owned();
  assert_eq!(body_bytes(plain).await, "plain javascript");

  let gz = send(
    &app,
    req_with("/app.3f2a9c1b.js", &[(header::ACCEPT_ENCODING, "gzip")]),
  )
  .await;
  assert_eq!(gz.headers()[header::CONTENT_ENCODING], "gzip");
  assert_eq!(gz.headers()[header::VARY], "accept-encoding");
  let gz_tag = header_str(&gz, header::ETAG).to_owned();
  assert!(gz_tag.ends_with("-gzip\""), "{gz_tag}");
  assert_ne!(gz_tag, plain_tag);
  assert_eq!(body_bytes(gz).await, "gzip bytes");

  let br = send(
    &app,
    req_with(
      "/app.3f2a9c1b.js",
      &[(header::ACCEPT_ENCODING, "gzip;q=0.5, br")],
    ),
  )
  .await;
  assert_eq!(br.headers()[header::CONTENT_ENCODING], "br");
  assert_eq!(body_bytes(br).await, "brotli bytes");

  // Without precompression, a fixed policy and no Vary.
  let app = Router::new().fallback_service(
    serve_dir(&root)
      .precompressed(false)
      .cache_control(CacheControl::public(std::time::Duration::from_secs(60))),
  );
  let res = send(
    &app,
    req_with("/app.3f2a9c1b.js", &[(header::ACCEPT_ENCODING, "gzip")]),
  )
  .await;
  assert!(res.headers().get(header::CONTENT_ENCODING).is_none());
  assert!(res.headers().get(header::VARY).is_none());
  assert_eq!(res.headers()[header::CACHE_CONTROL], "public, max-age=60");
}

#[tokio::test]
async fn unsafe_and_missing_paths_are_problems() {
  let root = site(
    "unsafe",
    &[
      ("public/ok.txt", b"ok"),
      ("public/.env", b"SECRET=1"),
      ("public/.well-known/security.txt", b"contact"),
      ("secret.txt", b"outside the root"),
    ],
  );
  let app = Router::new().nest_service("/static", serve_dir(root.join("public")));

  let res = send(&app, get_req("/static/ok.txt")).await;
  assert_eq!(body_bytes(res).await, "ok");
  let res = send(&app, get_req("/static/.well-known/security.txt")).await;
  assert_eq!(body_bytes(res).await, "contact");

  for uri in [
    "/static/.env",
    "/static/%2eenv",
    "/static/../secret.txt",
    "/static/%2e%2e/secret.txt",
    "/static/..%2fsecret.txt",
    "/static/missing.txt",
  ] {
    let body = problem(send(&app, get_req(uri)).await, StatusCode::NOT_FOUND).await;
    assert!(
      !body.to_string().contains("outside the root"),
      "{uri}: {body}"
    );
  }

  let post = Request::builder()
    .method("POST")
    .uri("/static/ok.txt")
    .body(Body::empty())
    .unwrap();
  let res = send(&app, post).await;
  assert_eq!(res.headers()[header::ALLOW], "GET, HEAD");
  problem(res, StatusCode::METHOD_NOT_ALLOWED).await;
}

#[tokio::test]
async fn preconditions_and_ranges() {
  let root = site("ranges", &[("data.bin", b"0123456789")]);
  let app = static_app(&root);
  let full = send(&app, get_req("/data.bin")).await;
  let etag = header_str(&full, header::ETAG).to_owned();
  let modified = header_str(&full, header::LAST_MODIFIED).to_owned();

  // If-Match: strong comparison.
  let res = send(&app, req_with("/data.bin", &[(header::IF_MATCH, &etag)])).await;
  assert_eq!(res.status(), StatusCode::OK);
  for condition in ["\"other\"".to_owned(), format!("W/{etag}")] {
    let res = send(
      &app,
      req_with("/data.bin", &[(header::IF_MATCH, &condition)]),
    )
    .await;
    problem(res, StatusCode::PRECONDITION_FAILED).await;
  }
  let res = send(
    &app,
    req_with(
      "/data.bin",
      &[(header::IF_UNMODIFIED_SINCE, "Thu, 01 Jan 1970 00:00:00 GMT")],
    ),
  )
  .await;
  problem(res, StatusCode::PRECONDITION_FAILED).await;

  // Range, with and without a matching If-Range.
  let res = send(&app, req_with("/data.bin", &[(header::RANGE, "bytes=2-4")])).await;
  assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
  assert_eq!(header_str(&res, header::ETAG), etag);
  assert_eq!(body_bytes(res).await, "234");
  for (if_range, status) in [
    (etag.as_str(), StatusCode::PARTIAL_CONTENT),
    (modified.as_str(), StatusCode::PARTIAL_CONTENT),
    ("\"stale\"", StatusCode::OK),
    ("Thu, 01 Jan 1970 00:00:00 GMT", StatusCode::OK),
  ] {
    let res = send(
      &app,
      req_with(
        "/data.bin",
        &[(header::RANGE, "bytes=2-4"), (header::IF_RANGE, if_range)],
      ),
    )
    .await;
    assert_eq!(res.status(), status, "If-Range: {if_range}");
  }
  let res = send(
    &app,
    req_with(
      "/gone.bin",
      &[(header::RANGE, "bytes=2-4"), (header::IF_RANGE, &etag)],
    ),
  )
  .await;
  problem(res, StatusCode::NOT_FOUND).await;
  let res = send(
    &app,
    req_with("/data.bin", &[(header::RANGE, "bytes=50-60")]),
  )
  .await;
  assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
}

// ── HttpCacheLayer (data.cache#7) ───────────────────────────────────────────────────────────

fn cached_app(layer: HttpCacheLayer) -> Router {
  Router::new()
    .route(
      "/text",
      get(|| async { "hello" }).post(|| async { "posted" }),
    )
    .route(
      "/own",
      get(|| async {
        (
          [
            (header::ETAG, "\"v7\""),
            (header::CACHE_CONTROL, "public, max-age=5"),
          ],
          "own validators",
        )
      }),
    )
    .route(
      "/secret",
      get(|| async { ([(header::CACHE_CONTROL, "no-store")], "secret") }),
    )
    .route(
      "/dated",
      get(|| async {
        (
          [(header::LAST_MODIFIED, "Wed, 21 Oct 2015 07:28:00 GMT")],
          "dated",
        )
      }),
    )
    .route("/large", get(|| async { "x".repeat(4096) }))
    .route(
      "/missing",
      get(|| async { (StatusCode::NOT_FOUND, "nope").into_response() }),
    )
    .layer(layer)
}

#[tokio::test]
async fn http_cache_layer_honours_handler_headers() {
  let app = cached_app(HttpCacheLayer::default());

  let res = send(
    &app,
    req_with("/own", &[(header::IF_NONE_MATCH, "W/\"v7\"")]),
  )
  .await;
  assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
  assert_eq!(res.headers()[header::ETAG], "\"v7\"");
  assert_eq!(res.headers()[header::CACHE_CONTROL], "public, max-age=5");

  let head = Request::builder()
    .method("HEAD")
    .uri("/own")
    .header(header::IF_NONE_MATCH, "\"v7\"")
    .body(Body::empty())
    .unwrap();
  assert_eq!(send(&app, head).await.status(), StatusCode::NOT_MODIFIED);

  // HEAD without a handler ETag: Cache-Control only, never a 304.
  let head = Request::builder()
    .method("HEAD")
    .uri("/text")
    .header(header::IF_NONE_MATCH, "*")
    .body(Body::empty())
    .unwrap();
  let res = send(&app, head).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(res.headers()[header::CACHE_CONTROL], "private, no-cache");
  assert!(res.headers().get(header::ETAG).is_none());

  let res = send(&app, req_with("/secret", &[(header::IF_NONE_MATCH, "*")])).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert!(res.headers().get(header::ETAG).is_none());
  assert_eq!(body_bytes(res).await, "secret");

  // If-Modified-Since against the handler's Last-Modified.
  for (since, status) in [
    ("Wed, 21 Oct 2015 07:28:00 GMT", StatusCode::NOT_MODIFIED),
    ("Thu, 22 Oct 2015 07:28:00 GMT", StatusCode::NOT_MODIFIED),
    ("Tue, 20 Oct 2015 07:28:00 GMT", StatusCode::OK),
    ("not a date", StatusCode::OK),
  ] {
    let res = send(
      &app,
      req_with("/dated", &[(header::IF_MODIFIED_SINCE, since)]),
    )
    .await;
    assert_eq!(res.status(), status, "{since}");
  }
  let res = send(
    &app,
    req_with(
      "/text",
      &[(header::IF_MODIFIED_SINCE, "Wed, 21 Oct 2015 07:28:00 GMT")],
    ),
  )
  .await;
  assert_eq!(res.status(), StatusCode::OK);

  // Other methods and statuses pass through untouched.
  let post = Request::builder()
    .method("POST")
    .uri("/text")
    .body(Body::empty())
    .unwrap();
  let res = send(&app, post).await;
  assert!(res.headers().get(header::CACHE_CONTROL).is_none());
  assert!(res.headers().get(header::ETAG).is_none());
  let res = send(&app, get_req("/missing")).await;
  assert_eq!(res.status(), StatusCode::NOT_FOUND);
  assert!(res.headers().get(header::ETAG).is_none());
}

#[tokio::test]
async fn http_cache_layer_options() {
  let app = cached_app(
    HttpCacheLayer::new()
      .cache_control(CacheControl::no_cache())
      .body_limit(1024)
      .weak_etags(),
  );
  let res = send(&app, get_req("/text")).await;
  assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
  let etag = header_str(&res, header::ETAG).to_owned();
  assert!(etag.starts_with("W/\""), "{etag}");
  let res = send(&app, req_with("/text", &[(header::IF_NONE_MATCH, &etag)])).await;
  assert_eq!(res.status(), StatusCode::NOT_MODIFIED);

  // Bodies above the limit stream through without an ETag.
  let res = send(&app, get_req("/large")).await;
  assert!(res.headers().get(header::ETAG).is_none());
  assert_eq!(body_bytes(res).await.len(), 4096);

  // Outside a compression layer, each encoding gets its own strong tag.
  let app = Router::new()
    .route("/large", get(|| async { "x".repeat(4096) }))
    .layer(CompressionLayer::new())
    .layer(HttpCacheLayer::new());
  let plain = send(&app, get_req("/large")).await;
  let gzip = send(
    &app,
    req_with("/large", &[(header::ACCEPT_ENCODING, "gzip")]),
  )
  .await;
  assert_eq!(gzip.headers()[header::CONTENT_ENCODING], "gzip");
  assert_ne!(plain.headers()[header::ETAG], gzip.headers()[header::ETAG]);
}
