//! Graceful shutdown: OS signals and in-flight drain.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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
}
