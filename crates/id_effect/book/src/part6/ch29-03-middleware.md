# Middleware, static files and HTTP caching

`id_effect_axum` ships the middleware a production HTTP service needs as tower layers with
working defaults: `CompressionLayer`, `RequestIdLayer`, `RateLimitLayer`, `BodyLimitLayer` and
`TimeoutLayer`. It also serves static files (`serve_dir`) with conditional GET, and caches
dynamic responses with `HttpCacheLayer`. Every rejection is an RFC 9457 `Problem`
(`application/problem+json`).

| Need | API | Default | Rejection |
|---|---|---|---|
| Compress responses | `middleware::CompressionLayer` | gzip, brotli, deflate for bodies of 860 bytes or more | — |
| Correlate requests | `middleware::RequestIdLayer`, extractor `RequestId` | keeps a valid incoming `x-request-id`, else generates one | — |
| Limit request rate | `middleware::RateLimitLayer` | token bucket per client IP | `429` + `Retry-After` |
| Limit body size | `middleware::BodyLimitLayer` | 2 MiB | `413` |
| Bound request time | `middleware::TimeoutLayer` | 30 s, the handler's fiber is interrupted | `503` |
| Cache dynamic responses | `middleware::HttpCacheLayer`, `CacheControl` | `private, no-cache` and a body-hash `ETag` | `304` |
| Serve files | `server::serve_dir` | `ETag`, `Last-Modified`, precompressed `.br` / `.gz` | `304`, `404`, `412` |

The runnable version of this chapter is
[`crates/id_effect_axum/examples/090_middleware.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_axum/examples/090_middleware.rs):
an API and a static site behind the whole stack.

```bash
cargo run -p id_effect_axum --example 090_middleware            # sends a few requests, prints them
cargo run -p id_effect_axum --example 090_middleware -- --serve # listens on 127.0.0.1:3000
```

## Stacking the layers

Stack the layers with `Router::layer`. The **last** call is the **outermost** layer. Put
`RequestIdLayer` outermost so every response carries `x-request-id`, rejections included. Then
come the rate limit, the body limit, the timeout and compression:

```rust
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use id_effect::{Effect, succeed};
use id_effect_axum::middleware::{
  BodyLimitLayer, CompressionLayer, REQUEST_ID_HEADER, RateLimitLayer, RequestId, RequestIdLayer,
  TimeoutLayer,
};
use id_effect_axum::routing;
use tower::ServiceExt;

fn hello(_: &mut ()) -> Effect<String, Infallible, ()> {
  succeed("hello".to_owned())
}

let app: Router = Router::new()
  .route("/hello", routing::get(hello))
  .route(
    "/whoami",
    axum::routing::get(|id: RequestId| async move { format!("request {id}") }),
  )
  .layer(CompressionLayer::new())
  .layer(TimeoutLayer::new(Duration::from_secs(10)))
  .layer(BodyLimitLayer::new(64 * 1024))
  .layer(RateLimitLayer::per_second(50.0, 100.0))
  .layer(RequestIdLayer::new());

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let mut req = Request::builder()
  .uri("/whoami")
  .header(REQUEST_ID_HEADER, "demo-1")
  .body(Body::empty())
  .unwrap();
// What `into_make_service_with_connect_info::<SocketAddr>()` adds when serving.
req.extensions_mut().insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4000))));
let res = app.oneshot(req).await.unwrap();
assert_eq!(res.status(), StatusCode::OK);
assert_eq!(res.headers()[REQUEST_ID_HEADER], "demo-1");
# });
```

The request id reaches handlers as the `RequestId` extractor. Each request runs inside an
`http.request` effect span with the method, the route template, the id and the status. The span
goes to the tracing buffer or, with `.span_hook(..)`, to OpenTelemetry.

## Timeouts interrupt the handler

`TimeoutLayer` does not just stop waiting. It interrupts the handler's fiber, so its `ensuring`
finalizers run and its scoped resources are released. No work outlives the `503`:

```rust
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use id_effect::{sleep, sync};
use id_effect_axum::middleware::TimeoutLayer;
use id_effect_axum::{PROBLEM_JSON, routing};
use tower::ServiceExt;

let released = Arc::new(AtomicBool::new(false));
let flag = released.clone();
let app: Router = Router::new()
  .route(
    "/slow",
    routing::get(move |_: &mut ()| {
      let flag = flag.clone();
      sleep::<Infallible, ()>(Duration::from_secs(30))
        .map(|()| "done")
        .ensuring(sync(move || flag.store(true, Ordering::SeqCst)))
    }),
  )
  .layer(TimeoutLayer::new(Duration::from_millis(50)));

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let req = Request::builder().uri("/slow").body(Body::empty()).unwrap();
let res = app.oneshot(req).await.unwrap();
assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
assert_eq!(res.headers()["content-type"], PROBLEM_JSON);
while !released.load(Ordering::SeqCst) {
  tokio::time::sleep(Duration::from_millis(5)).await;
}
# });
```

## Rate limiting

`RateLimitLayer::per_second(rate, burst)` limits each client IP. The IP comes from
`into_make_service_with_connect_info::<SocketAddr>()`; without it, all requests share one
bucket. To key by principal, use `RateLimitKey::from_fn(..).or(RateLimitKey::peer_ip())`. To
share buckets across instances, implement your own `RateLimitStore`. A store's `acquire` is an
effect, so it can do I/O:

```rust
use std::sync::atomic::{AtomicU32, Ordering};

use axum::{Router, routing::get};
use id_effect::{Effect, HNil, fail, succeed};
use id_effect_axum::middleware::{RateLimitError, RateLimitKey, RateLimitLayer, RateLimitStore};

/// A stand-in for a shared store (e.g. Redis `INCR` with a TTL): 100 requests in total.
#[derive(Default)]
struct SharedStore {
  used: AtomicU32,
}

impl RateLimitStore for SharedStore {
  fn acquire(&self, _key: &str) -> Effect<(), RateLimitError, HNil> {
    if self.used.fetch_add(1, Ordering::SeqCst) < 100 {
      succeed(())
    } else {
      fail(RateLimitError)
    }
  }
}

#[derive(Clone)]
struct Principal(String);

let by_user = RateLimitKey::from_fn(|req| {
  req.extensions().get::<Principal>().map(|p| format!("user:{}", p.0))
});
let app: Router = Router::new()
  .route("/api", get(|| async { "ok" }))
  .layer(RateLimitLayer::new(SharedStore::default()).key(by_user.or(RateLimitKey::peer_ip())));
# let _ = app;
```

## Static files

`server::serve_dir(root)` serves a directory as a tower `Service`. Mount it with
`Router::nest_service` or as the router's `fallback_service`. It is built on tower-http's
`ServeDir` and adds the parts a cache needs:

- **Validators.** Every file has `Last-Modified` and a strong `ETag` built from its modification
  time, size and encoding. A matching `If-None-Match` gets `304 Not Modified`, and so does an
  `If-Modified-Since` that is not older than the file. When both headers are present,
  `If-None-Match` decides. `If-Match` and `If-Unmodified-Since` answer `412`. `If-Range` decides
  whether a `Range` request is honoured.
- **Precompressed variants.** If the client accepts `br` or `gzip` and `app.js.br` or `app.js.gz`
  exists next to `app.js`, that file is sent with `Content-Encoding` set. Every response carries
  `Vary: Accept-Encoding`.
- **Cache-Control.** Content-hashed names such as `app.3f2a9c1b.js` get
  `public, max-age=31536000, immutable`. Everything else, HTML included, gets `no-cache`, so the
  browser revalidates it with the `ETag` on each use. To use one policy for every file, call
  `.cache_control(CacheControl::..)`.
- **Safe paths.** Requests cannot leave the root through `..` segments or absolute paths. Hidden
  files (`.env`, `.git/`, percent-encoded or not) are never served; `.well-known` is the one
  exception. A miss is a `404` Problem.

```rust
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use id_effect_axum::server::serve_dir;
use tower::ServiceExt;

let public = std::env::temp_dir().join(format!("book_ch29_03_{}", std::process::id()));
std::fs::create_dir_all(&public).unwrap();
std::fs::write(public.join("index.html"), "<h1>hello</h1>").unwrap();
std::fs::write(public.join("app.3f2a9c1b.js"), "console.log(1)").unwrap();
std::fs::write(public.join(".env"), "SECRET=1").unwrap();

let app: Router = Router::new().fallback_service(serve_dir(&public));

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let get = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();

let page = app.clone().oneshot(get("/")).await.unwrap();
assert_eq!(page.status(), StatusCode::OK);
assert_eq!(page.headers()[header::CACHE_CONTROL], "no-cache");
let etag = page.headers()[header::ETAG].clone();

// The browser revalidates: same file, no body.
let mut revalidate = get("/");
revalidate.headers_mut().insert(header::IF_NONE_MATCH, etag);
let res = app.clone().oneshot(revalidate).await.unwrap();
assert_eq!(res.status(), StatusCode::NOT_MODIFIED);

let script = app.clone().oneshot(get("/app.3f2a9c1b.js")).await.unwrap();
assert_eq!(script.headers()[header::CACHE_CONTROL], "public, max-age=31536000, immutable");

for hidden in ["/.env", "/%2eenv", "/../etc/passwd"] {
  let res = app.clone().oneshot(get(hidden)).await.unwrap();
  assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
# });
# std::fs::remove_dir_all(&public).unwrap();
```

## Caching dynamic responses

`HttpCacheLayer` does the same for handlers. For a `200` answer to `GET` it:

1. sets its `Cache-Control`, unless the handler set one;
2. keeps a handler's own `ETag`, or computes a strong one from a SHA-256 hash of the body. Only
   bodies up to `body_limit` (1 MiB by default) are hashed; larger or streaming bodies pass
   through without an `ETag`;
3. answers a matching `If-None-Match` with `304`. If the request has no `If-None-Match`, it
   compares `If-Modified-Since` with the handler's `Last-Modified` instead.

The handler still runs for a conditional request. The layer saves bandwidth, not work. Responses
marked `no-store` are left alone. Put the layer **outside** `CompressionLayer`, so the `ETag` is
the hash of the bytes actually sent and each encoding gets its own tag. If the layer must sit
inside compression, use `.weak_etags()`.

```rust
use std::convert::Infallible;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use id_effect::{Effect, succeed};
use id_effect_axum::middleware::{CacheControl, CompressionLayer, HttpCacheLayer};
use id_effect_axum::routing;
use tower::ServiceExt;

fn catalog(_: &mut ()) -> Effect<&'static str, Infallible, ()> {
  succeed(r#"[{"sku":"tea"}]"#)
}

let app: Router = Router::new()
  .route("/catalog", routing::get(catalog))
  .layer(CompressionLayer::new())
  .layer(HttpCacheLayer::new().cache_control(CacheControl::private(Duration::from_secs(60))));

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let get = || Request::builder().uri("/catalog").body(Body::empty()).unwrap();
let first = app.clone().oneshot(get()).await.unwrap();
assert_eq!(first.headers()[header::CACHE_CONTROL], "private, max-age=60");
let etag = first.headers()[header::ETAG].clone();

let mut again = get();
again.headers_mut().insert(header::IF_NONE_MATCH, etag);
let res = app.clone().oneshot(again).await.unwrap();
assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
# });
```

`CacheControl` has the common presets: `no_cache()`, `private_no_cache()` (the layer's default,
safe for per-user responses), `no_store()`, `immutable()`, `public(max_age)` and
`private(max_age)`. For any other directive list, use `from_static("...")`.

## What to remember

- Layers stack with `Router::layer`, and the last call is the outermost. Put `RequestIdLayer`
  outermost.
- A timeout interrupts the handler's fiber, so its finalizers run.
- `RateLimitLayer::per_second` needs connect info to tell clients apart.
- `serve_dir` gives files an `ETag` and `Last-Modified`, answers `304`, serves precompressed
  variants and never serves hidden files or paths outside the root.
- `HttpCacheLayer` gives dynamic `GET` responses `Cache-Control` and a body-hash `ETag`, and
  answers `If-None-Match` with `304`.
