//! A production HTTP stack: `Effect` API routes and static files behind the first-party
//! middleware, with HTTP caching.
//!
//! - `/api/*`: `Effect` handlers behind [`HttpCacheLayer`] (`Cache-Control` + a body-hash
//!   `ETag`, `304` for a matching `If-None-Match`).
//! - everything else: [`serve_dir`] over a `public/` directory (`ETag` / `Last-Modified` / `304`,
//!   precompressed `.gz` / `.br` variants, `immutable` for hashed asset names, `no-cache` for HTML).
//! - around both: compression, timeout, body limit, rate limit and request id, outermost last.
//!
//! Run: `cargo run -p id_effect_axum --example 090_middleware` (writes a small site to a
//! temporary directory, sends a few requests and exits), or add `-- --serve` to listen on
//! `127.0.0.1:3000` until Ctrl-C:
//!
//! ```text
//! curl -i localhost:3000/
//! curl -i -H 'if-none-match: <etag from above>' localhost:3000/
//! curl -i localhost:3000/api/catalog
//! ```

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use id_effect::{Effect, succeed};
use id_effect_axum::middleware::{
  BodyLimitLayer, CacheControl, CompressionLayer, HttpCacheLayer, REQUEST_ID_HEADER,
  RateLimitLayer, RequestIdLayer, TimeoutLayer,
};
use id_effect_axum::routing;
use id_effect_axum::server::serve_dir;
use tower::ServiceExt;

fn catalog(_: &mut ()) -> Effect<String, Infallible, ()> {
  succeed(r#"[{"sku":"tea","price":4},{"sku":"cake","price":3}]"#.to_owned())
}

/// The API: `Effect` handlers with HTTP caching (a minute in the browser, then revalidate).
fn api() -> Router {
  Router::new()
    .route("/catalog", routing::get(catalog))
    .layer(HttpCacheLayer::new().cache_control(CacheControl::private(Duration::from_secs(60))))
}

/// The whole application with the recommended middleware order (the last `layer` is outermost).
pub fn app(public: &Path) -> Router {
  Router::new()
    .nest("/api", api())
    .fallback_service(serve_dir(public))
    .layer(CompressionLayer::new())
    .layer(TimeoutLayer::new(Duration::from_secs(10)))
    .layer(BodyLimitLayer::new(64 * 1024))
    .layer(RateLimitLayer::per_second(50.0, 100.0))
    .layer(RequestIdLayer::new())
}

/// A small built site: an HTML entry point and a content-hashed script with a gzip variant.
fn write_site() -> PathBuf {
  let public = std::env::temp_dir().join(format!("id_effect_090_public_{}", std::process::id()));
  std::fs::create_dir_all(&public).unwrap();
  std::fs::write(
    public.join("index.html"),
    r#"<!doctype html><script src="/app.3f2a9c1b.js"></script>"#,
  )
  .unwrap();
  std::fs::write(public.join("app.3f2a9c1b.js"), "console.log('hi')").unwrap();
  std::fs::write(public.join("app.3f2a9c1b.js.gz"), "(gzip bytes)").unwrap();
  public
}

async fn send(app: &Router, mut req: Request<Body>) -> Response {
  req
    .extensions_mut()
    .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50_000))));
  let line = format!("{} {}", req.method(), req.uri());
  let res = app.clone().oneshot(req).await.unwrap();
  let show = |name| {
    res
      .headers()
      .get(name)
      .map_or("-", |v| v.to_str().unwrap())
      .to_owned()
  };
  println!(
    "{line} -> {}\n  etag: {}\n  cache-control: {}\n  content-encoding: {}\n  x-request-id: {}",
    res.status(),
    show(header::ETAG),
    show(header::CACHE_CONTROL),
    show(header::CONTENT_ENCODING),
    show(header::HeaderName::from_static(REQUEST_ID_HEADER)),
  );
  res
}

fn get(uri: &str, headers: &[(header::HeaderName, &str)]) -> Request<Body> {
  let mut req = Request::builder().uri(uri);
  for (name, value) in headers {
    req = req.header(name, *value);
  }
  req.body(Body::empty()).unwrap()
}

async fn demo(app: &Router) {
  // Static HTML: no-cache, revalidated with its ETag.
  let page = send(app, get("/", &[])).await;
  assert_eq!(page.status(), StatusCode::OK);
  assert_eq!(page.headers()[header::CACHE_CONTROL], "no-cache");
  let etag = page.headers()[header::ETAG].to_str().unwrap().to_owned();
  let again = send(app, get("/", &[(header::IF_NONE_MATCH, &etag)])).await;
  assert_eq!(again.status(), StatusCode::NOT_MODIFIED);

  // A hashed asset: immutable, and the precompressed variant for a gzip client.
  let script = send(
    app,
    get("/app.3f2a9c1b.js", &[(header::ACCEPT_ENCODING, "gzip")]),
  )
  .await;
  assert_eq!(
    script.headers()[header::CACHE_CONTROL],
    "public, max-age=31536000, immutable"
  );
  assert_eq!(script.headers()[header::CONTENT_ENCODING], "gzip");

  // Hidden files and traversal never leave the site.
  for path in ["/../secret", "/.env"] {
    assert_eq!(
      send(app, get(path, &[])).await.status(),
      StatusCode::NOT_FOUND
    );
  }

  // The API: an ETag from the body, then 304.
  let res = send(app, get("/api/catalog", &[])).await;
  assert_eq!(res.headers()[header::CACHE_CONTROL], "private, max-age=60");
  let etag = res.headers()[header::ETAG].to_str().unwrap().to_owned();
  let body = res.into_body().collect().await.unwrap().to_bytes();
  println!("  body: {}", String::from_utf8_lossy(&body));
  let res = send(app, get("/api/catalog", &[(header::IF_NONE_MATCH, &etag)])).await;
  assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::main]
async fn main() {
  let public = write_site();
  let app = app(&public);
  if std::env::args().any(|a| a == "--serve") {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
      .await
      .unwrap();
    println!("listening on http://{}", listener.local_addr().unwrap());
    axum::serve(
      listener,
      app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
      let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .unwrap();
  } else {
    demo(&app).await;
    println!("090_middleware ok");
  }
  let _ = std::fs::remove_dir_all(public);
}
