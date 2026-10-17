//! Graceful shutdown: OS signals and in-flight drain.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::middleware::{self, Next};
use http_body::{Frame, SizeHint};
use tokio::time;

/// Why the host is shutting down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownReason {
  /// Ctrl+C / SIGINT.
  CtrlC,
  /// SIGTERM (Unix).
  Sigterm,
}

/// Tracks in-flight work for drain-on-shutdown.
#[derive(Debug, Default)]
pub struct HostDrain {
  in_flight: AtomicUsize,
}

impl HostDrain {
  /// Create an empty drain counter.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Increment in-flight work (call when accepting new work).
  #[inline]
  pub fn enter(&self) {
    self.in_flight.fetch_add(1, Ordering::SeqCst);
  }

  /// Decrement in-flight work (call when work completes).
  #[inline]
  pub fn leave(&self) {
    self.in_flight.fetch_sub(1, Ordering::SeqCst);
  }

  /// Current in-flight count.
  #[inline]
  pub fn in_flight(&self) -> usize {
    self.in_flight.load(Ordering::SeqCst)
  }
}

/// One unit of in-flight work: entered on creation, left on drop.
struct InFlight(Arc<HostDrain>);

impl InFlight {
  fn enter(drain: Arc<HostDrain>) -> Self {
    drain.enter();
    Self(drain)
  }
}

impl Drop for InFlight {
  fn drop(&mut self) {
    self.0.leave();
  }
}

/// A response body that holds its request's [`InFlight`] until it is sent or dropped.
struct InFlightBody {
  inner: Body,
  _in_flight: InFlight,
}

impl http_body::Body for InFlightBody {
  type Data = Bytes;
  type Error = axum::Error;

  fn poll_frame(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
    Pin::new(&mut self.get_mut().inner).poll_frame(cx)
  }

  fn is_end_stream(&self) -> bool {
    self.inner.is_end_stream()
  }

  fn size_hint(&self) -> SizeHint {
    self.inner.size_hint()
  }
}

/// Counts every request through `router` in `drain`, from its arrival until its response body
/// is sent (or dropped), so [`drain_with_timeout`] waits for it. Wrap the router given to
/// [`serve_router`](crate::serve_router) with `host.drain`.
pub fn track_in_flight(router: Router, drain: &Arc<HostDrain>) -> Router {
  let drain = Arc::clone(drain);
  router.layer(middleware::from_fn(move |request: Request, next: Next| {
    let in_flight = InFlight::enter(Arc::clone(&drain));
    async move {
      let response = next.run(request).await;
      response.map(|inner| {
        Body::new(InFlightBody {
          inner,
          _in_flight: in_flight,
        })
      })
    }
  }))
}

/// Wait for Ctrl+C and, on Unix, SIGTERM.
pub async fn wait_for_shutdown() -> ShutdownReason {
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("register SIGTERM");
    tokio::select! {
      _ = tokio::signal::ctrl_c() => ShutdownReason::CtrlC,
      _ = sigterm.recv() => ShutdownReason::Sigterm,
    }
  }
  #[cfg(not(unix))]
  {
    tokio::signal::ctrl_c()
      .await
      .expect("register Ctrl+C handler");
    ShutdownReason::CtrlC
  }
}

/// Poll until `drain` is idle or `timeout` elapses.
pub async fn drain_with_timeout(drain: &HostDrain, timeout: Duration) {
  let deadline = time::Instant::now() + timeout;
  while drain.in_flight() > 0 && time::Instant::now() < deadline {
    time::sleep(Duration::from_millis(25)).await;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn drain_tracks_in_flight() {
    let drain = HostDrain::new();
    assert_eq!(drain.in_flight(), 0);
    drain.enter();
    drain.enter();
    assert_eq!(drain.in_flight(), 2);
    drain.leave();
    assert_eq!(drain.in_flight(), 1);
  }

  #[tokio::test]
  async fn drain_with_timeout_returns_when_idle() {
    let drain = HostDrain::new();
    drain_with_timeout(&drain, Duration::from_millis(50)).await;
  }

  #[tokio::test]
  async fn drain_with_timeout_elapses_while_in_flight() {
    let drain = HostDrain::new();
    drain.enter();
    drain_with_timeout(&drain, Duration::from_millis(30)).await;
    assert_eq!(drain.in_flight(), 1);
    drain.leave();
  }

  #[tokio::test]
  async fn track_in_flight_counts_a_request_until_its_body_is_consumed() {
    use axum::routing::get;
    use http_body::Body as _;
    use http_body_util::BodyExt;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    let drain = Arc::new(HostDrain::new());
    let release = Arc::new(Notify::new());
    let gate = Arc::clone(&release);
    let app = track_in_flight(
      Router::new().route(
        "/slow",
        get(move || {
          let gate = Arc::clone(&gate);
          async move {
            gate.notified().await;
            "done"
          }
        }),
      ),
      &drain,
    );
    let request = axum::http::Request::builder()
      .uri("/slow")
      .body(Body::empty())
      .unwrap();
    let call = tokio::spawn(app.oneshot(request));
    while drain.in_flight() == 0 {
      time::sleep(Duration::from_millis(5)).await;
    }
    release.notify_one();
    let response = call.await.unwrap().unwrap();
    // Answered, but the body is not sent yet; its length is still known (`content-length`).
    assert_eq!(drain.in_flight(), 1);
    assert_eq!(response.body().size_hint().exact(), Some(4));
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"done");
    assert_eq!(drain.in_flight(), 0);
  }
}
