//! Graceful OpenTelemetry flush/shutdown helpers for Tokio hosts.

use std::future::Future;
use std::time::Duration;

use crate::starter::OtelStarterGuard;

/// Flush all OTEL signals, then shut down providers.
#[inline]
pub fn graceful_otel_shutdown(guard: OtelStarterGuard) {
  guard.force_flush();
  guard.shutdown();
}

/// Wait for Ctrl+C or SIGTERM (Unix), flush OTEL, then shut down.
pub async fn shutdown_otel_on_signal(guard: OtelStarterGuard) {
  let _ = shutdown_signal().await;
  graceful_otel_shutdown(guard);
}

/// Wait for Ctrl+C or SIGTERM (Unix), flush OTEL within `timeout`, then shut down.
pub async fn shutdown_otel_on_signal_with_timeout(guard: OtelStarterGuard, timeout: Duration) {
  let _ = shutdown_signal().await;
  let flush = tokio::time::timeout(timeout, async {
    guard.force_flush();
  });
  let _ = flush.await;
  guard.shutdown();
}

async fn shutdown_signal() {
  let ctrl_c = async {
    tokio::signal::ctrl_c()
      .await
      .expect("failed to install Ctrl+C handler");
  };

  #[cfg(unix)]
  let terminate = async {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
      .expect("failed to install SIGTERM handler")
      .recv()
      .await;
  };

  #[cfg(not(unix))]
  let terminate = std::future::pending::<()>();

  tokio::select! {
    () = ctrl_c => {},
    () = terminate => {},
  }
}

/// Run `serve` until `shutdown` resolves, then flush OTEL providers.
pub async fn run_until_shutdown<F, Fut>(
  guard: OtelStarterGuard,
  serve: F,
  shutdown: impl Future<Output = ()>,
) where
  F: FnOnce() -> Fut,
  Fut: Future<Output = ()>,
{
  tokio::select! {
    () = serve() => {},
    () = shutdown => {},
  }
  graceful_otel_shutdown(guard);
}
