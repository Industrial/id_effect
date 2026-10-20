//! `Alt-Svc` (RFC 7838) on TCP responses: tells HTTP clients that the same origin also speaks
//! HTTP/3 on a UDP port, so their next request can upgrade.
//!
//! The host adds [`AltSvcLayer`] to the TCP router by itself when it serves HTTP/3 next to TLS
//! on TCP (feature `http3`, [`HostConfig::http3`](crate::server::HostConfig::http3)): the
//! header is `h3=":<port>"; ma=86400` with the port both listeners share. Browsers honour
//! `Alt-Svc` only on HTTPS responses, so the host never sends it over plain HTTP. Behind a load
//! balancer that terminates HTTP/3 itself, put the layer on the router with the balancer's port.
//!
//! A response that already carries an `Alt-Svc` header (a handler's own) keeps it.
//!
//! ```rust
//! use axum::Router;
//! use id_effect_axum::server::alt_svc::AltSvcLayer;
//!
//! let layer = AltSvcLayer::h3(443);
//! assert_eq!(layer.value(), "h3=\":443\"; ma=86400");
//! let _app: Router = Router::new().layer(layer);
//! ```

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::http::{HeaderValue, Request, Response, header};
use tower_layer::Layer;
use tower_service::Service;

/// How long clients may remember the alternative: `ma=86400` (one day).
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(86_400);

/// Adds `Alt-Svc: h3=":<port>"; ma=<seconds>` to every response that has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltSvcLayer {
  value: HeaderValue,
}

impl AltSvcLayer {
  /// HTTP/3 on UDP `port` of the same host, remembered for [`DEFAULT_MAX_AGE`].
  pub fn h3(port: u16) -> Self {
    Self::h3_with_max_age(port, DEFAULT_MAX_AGE)
  }

  /// HTTP/3 on UDP `port`, remembered for `max_age` (whole seconds).
  pub fn h3_with_max_age(port: u16, max_age: Duration) -> Self {
    let value = format!("h3=\":{port}\"; ma={}", max_age.as_secs());
    Self {
      value: HeaderValue::try_from(value)
        .expect("digits, quotes, colons, semicolons and spaces form a valid header value"),
    }
  }

  /// The header value sent.
  pub fn value(&self) -> &HeaderValue {
    &self.value
  }
}

impl<S> Layer<S> for AltSvcLayer {
  type Service = AltSvc<S>;

  fn layer(&self, inner: S) -> Self::Service {
    AltSvc {
      inner,
      value: self.value.clone(),
    }
  }
}

/// The service of [`AltSvcLayer`].
#[derive(Clone, Debug)]
pub struct AltSvc<S> {
  inner: S,
  value: HeaderValue,
}

impl<S, B, R> Service<Request<B>> for AltSvc<S>
where
  S: Service<Request<B>, Response = Response<R>>,
  S::Future: Send + 'static,
{
  type Response = Response<R>;
  type Error = S::Error;
  type Future = Pin<Box<dyn Future<Output = Result<Response<R>, S::Error>> + Send>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, request: Request<B>) -> Self::Future {
    let response = self.inner.call(request);
    let value = self.value.clone();
    Box::pin(async move {
      let mut response = response.await?;
      response
        .headers_mut()
        .entry(header::ALT_SVC)
        .or_insert(value);
      Ok(response)
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::body::Body;
  use axum::routing::get;
  use tower_service::Service;

  async fn alt_svc(app: Router, uri: &str) -> Option<String> {
    let response = app
      .clone()
      .call(Request::get(uri).body(Body::empty()).unwrap())
      .await
      .unwrap();
    response
      .headers()
      .get(header::ALT_SVC)
      .map(|v| v.to_str().unwrap().to_owned())
  }

  #[tokio::test]
  async fn adds_the_header_and_keeps_a_handlers_own() {
    let app = Router::new()
      .route("/", get(|| async { "hi" }))
      .route(
        "/own",
        get(|| async { ([(header::ALT_SVC, "clear")], "own") }),
      )
      .layer(AltSvcLayer::h3(8443));
    assert_eq!(
      alt_svc(app.clone(), "/").await.as_deref(),
      Some("h3=\":8443\"; ma=86400")
    );
    assert_eq!(alt_svc(app.clone(), "/own").await.as_deref(), Some("clear"));
    // Fallback responses (404) carry it too.
    assert!(alt_svc(app, "/missing").await.is_some());
  }

  #[test]
  fn max_age_is_whole_seconds() {
    let layer = AltSvcLayer::h3_with_max_age(443, Duration::from_millis(60_500));
    assert_eq!(layer.value(), "h3=\":443\"; ma=60");
    assert_eq!(layer.clone(), layer);
  }
}
