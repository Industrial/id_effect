//! The first-party middleware stack: request id + span, rate limit, body limit, timeout and
//! compression around `Effect` handlers.
//!
//! - [`RequestIdLayer`] keeps (or generates) `x-request-id`, echoes it, hands it to handlers as
//!   [`RequestId`] and runs each request in an `http.request` span (method, route, id, status).
//! - [`RateLimitLayer`] answers `429` once a client's token bucket is empty.
//! - [`BodyLimitLayer`] answers `413` for bodies above the limit.
//! - [`TimeoutLayer`] answers `503` when a handler is too slow, interrupting its fiber.
//! - [`CompressionLayer`] gzip/brotli/deflate-encodes large responses.
//!
//! Every rejection is an RFC 9457 Problem (`application/problem+json`).
//!
//! Run: `cargo run -p id_effect_axum --example 091_middleware_layers` (sends a few requests and
//! exits), or add `-- --serve` to listen on `127.0.0.1:3000` until Ctrl-C:
//!
//! ```text
//! curl -i -H 'x-request-id: demo-1' localhost:3000/hello
//! curl -i --compressed localhost:3000/report
//! curl -i localhost:3000/slow
//! ```

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use id_effect::{Effect, TracingConfig, install_tracing_layer, sleep, snapshot_tracing, succeed};
use id_effect_axum::middleware::{
  BodyLimitLayer, CompressionLayer, REQUEST_ID_HEADER, REQUEST_SPAN, RateLimitLayer, RequestId,
  RequestIdLayer, TimeoutLayer,
};
use id_effect_axum::routing;
use tower::ServiceExt;

fn hello(_: &mut ()) -> Effect<String, Infallible, ()> {
  succeed("hello".to_owned())
}

fn report(_: &mut ()) -> Effect<String, Infallible, ()> {
  succeed("line of a long report\n".repeat(200))
}

fn slow(_: &mut ()) -> Effect<&'static str, Infallible, ()> {
  sleep(Duration::from_secs(60)).map(|()| "never")
}

/// The application with the recommended middleware order (the last `layer` is outermost).
pub fn app() -> Router {
  Router::new()
    .route("/hello", routing::get(hello))
    .route("/report", routing::get(report))
    .route("/slow", routing::get(slow))
    .route(
      "/whoami",
      axum::routing::get(|id: RequestId| async move { format!("you are request {id}") }),
    )
    .route(
      "/upload",
      axum::routing::post(|body: String| async move { format!("{} bytes", body.len()) }),
    )
    .layer(CompressionLayer::new())
    .layer(TimeoutLayer::new(Duration::from_millis(200)))
    .layer(BodyLimitLayer::new(1024))
    .layer(RateLimitLayer::per_second(0.1, 8.0))
    .layer(RequestIdLayer::new())
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
  let method = req.method().clone();
  let uri = req.uri().clone();
  let mut req = req;
  req
    .extensions_mut()
    .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50_000))));
  let res = app.clone().oneshot(req).await.unwrap();
  let status = res.status();
  let id = res.headers()[REQUEST_ID_HEADER]
    .to_str()
    .unwrap()
    .to_owned();
  let encoding = res
    .headers()
    .get(header::CONTENT_ENCODING)
    .map(|v| v.to_str().unwrap().to_owned());
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  let shown = match encoding {
    Some(enc) => format!("<{} bytes, {enc}>", bytes.len()),
    None => String::from_utf8_lossy(&bytes).into_owned(),
  };
  println!("{method} {uri} -> {status} (x-request-id: {id})\n{shown}\n");
  (status, id)
}

fn get(uri: &str) -> Request<Body> {
  Request::builder().uri(uri).body(Body::empty()).unwrap()
}

async fn demo(app: &Router) {
  install_tracing_layer(TracingConfig::enabled());

  let req = Request::builder()
    .uri("/hello")
    .header(REQUEST_ID_HEADER, "demo-1")
    .body(Body::empty())
    .unwrap();
  assert_eq!(send(app, req).await, (StatusCode::OK, "demo-1".to_owned()));
  assert_eq!(send(app, get("/whoami")).await.0, StatusCode::OK);

  let req = Request::builder()
    .uri("/report")
    .header(header::ACCEPT_ENCODING, "gzip")
    .body(Body::empty())
    .unwrap();
  assert_eq!(send(app, req).await.0, StatusCode::OK);

  let req = Request::builder()
    .method("POST")
    .uri("/upload")
    .header(header::CONTENT_LENGTH, 4096)
    .body(Body::from(vec![b'x'; 4096]))
    .unwrap();
  assert_eq!(send(app, req).await.0, StatusCode::PAYLOAD_TOO_LARGE);

  assert_eq!(
    send(app, get("/slow")).await.0,
    StatusCode::SERVICE_UNAVAILABLE
  );

  // The client's bucket holds 8 tokens (one more every 10 s) and five were taken: the next three
  // pass, then 429.
  for _ in 0..3 {
    assert_eq!(send(app, get("/hello")).await.0, StatusCode::OK);
  }
  assert_eq!(
    send(app, get("/hello")).await.0,
    StatusCode::TOO_MANY_REQUESTS
  );

  let spans = snapshot_tracing().spans;
  let span = spans
    .iter()
    .find(|s| s.name == REQUEST_SPAN)
    .expect("request span");
  println!("{REQUEST_SPAN} span record: {:?}", span.annotations);
}

#[tokio::main]
async fn main() {
  let app = app();
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
    println!("091_middleware_layers ok");
  }
}
