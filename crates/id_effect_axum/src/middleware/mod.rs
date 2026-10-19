//! First-party HTTP middleware: response compression, request ids with a per-request span,
//! request timeouts, body-size limits, load shedding and rate limiting.
//!
//! Every layer is a tower [`Layer`](tower_layer::Layer) applied with `Router::layer`, with
//! defaults that need no configuration. Rejections are RFC 9457 [`Problem`] responses
//! (`application/problem+json`):
//!
//! | Layer | Default | Rejection |
//! |---|---|---|
//! | [`CompressionLayer`] | gzip, brotli, deflate for bodies of at least 860 bytes | — |
//! | [`RequestIdLayer`] | keeps a valid incoming `x-request-id`, else generates one; one `http.request` span per request | — |
//! | [`TimeoutLayer`] | 30 s; the handler's fiber is interrupted (its finalizers run) | `503` |
//! | [`BodyLimitLayer`] | 2 MiB, checked on `Content-Length` and while the body streams | `413` |
//! | [`LoadShedLayer`] | [`LoadShed::DEFAULT_PER_CPU`] requests in flight per CPU, no queue; installed by the server entry points | `503` + `Retry-After` |
//! | [`RateLimitLayer`] | token bucket per client IP over [`id_effect_resilience::RateLimiter`] | `429` + `Retry-After` |
//! | [`HttpCacheLayer`] | `Cache-Control: private, no-cache` and a body-hash `ETag` on `GET` `200`s | `304` |
//!
//! Static files with `ETag` / `Last-Modified` / `304` are [`crate::server::serve_dir`].
//!
//! CORS, CSRF, CSP and security headers live in [`crate::server::security`]
//! ([`SecurityLayer::recommended`](crate::SecurityLayer::recommended)).
//!
//! `Router::layer` wraps the routes in the order given, the last call outermost. A sensible
//! stack puts the request id outermost (so rejections carry it too), then the rate limit, the
//! body limit, the timeout and compression:
//!
//! ```rust
//! use std::time::Duration;
//!
//! use axum::{Router, routing::get};
//! use id_effect_axum::middleware::{
//!   BodyLimitLayer, CompressionLayer, RateLimitLayer, RequestIdLayer, TimeoutLayer,
//! };
//!
//! let app: Router = Router::new()
//!   .route("/", get(|| async { "hello" }))
//!   .layer(CompressionLayer::new())
//!   .layer(TimeoutLayer::new(Duration::from_secs(10)))
//!   .layer(BodyLimitLayer::new(64 * 1024))
//!   .layer(RateLimitLayer::per_second(50.0, 100.0))
//!   .layer(RequestIdLayer::new());
//! # let _ = app;
//! ```

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::route::Problem;

pub mod compression;
pub mod http_cache;
pub mod limits;
pub mod load_shed;
pub mod rate_limit;
pub mod request_id;

pub use compression::{
  CompressionLayer, CompressionLevel, CompressionPredicate, CompressionService,
};
pub use http_cache::{CacheControl, DEFAULT_ETAG_BODY_LIMIT, HttpCacheLayer, HttpCacheService};
pub use limits::{
  BodyLimitLayer, BodyLimitService, DEFAULT_BODY_LIMIT, DEFAULT_TIMEOUT, TimeoutLayer,
  TimeoutService,
};
pub use load_shed::{LoadShed, LoadShedLayer, LoadShedService};
pub use rate_limit::{
  MemoryRateLimitStore, RateLimitError, RateLimitKey, RateLimitLayer, RateLimitService,
  RateLimitStore, RateLimiter,
};
pub use request_id::{
  REQUEST_ID_HEADER, REQUEST_SPAN, RequestId, RequestIdLayer, RequestIdService,
};

/// A [`Problem`] response for `status` with `detail`, its `instance` the request path.
pub(crate) fn problem_response(status: StatusCode, detail: String, path: &str) -> Response {
  Problem::new(status)
    .with_detail(detail)
    .with_instance(path)
    .into_response()
}
