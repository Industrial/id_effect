//! Deadlines and cancellation of one call (design note §2, "Rules common to every kind").
//!
//! Every call runs under a [`CallControl`]: a deadline the server caps
//! ([`effective_deadline`]: the client's `x-rpc-deadline-ms`, clamped to the server's maximum,
//! default 30 s) and an optional [`CancelSignal`] (the `cancel` frame of the multiplexed
//! transports). [`controlled`] races the call against both; when either wins, the call's future
//! is **dropped**, which interrupts the handler fiber, and its finalizers run (the 0.11 executor's
//! `DropPolicy::Interrupt`). A client disconnect needs no signal: the transport drops the call's
//! future with the connection, with the same effect.
//!
//! ```rust
//! use std::time::Duration;
//! use id_effect_rpc::cancel::{DEADLINE_HEADER, effective_deadline, requested_deadline};
//!
//! let mut headers = axum::http::HeaderMap::new();
//! headers.insert(DEADLINE_HEADER, "120000".parse().unwrap());
//! let asked = requested_deadline(&headers);
//! assert_eq!(asked, Some(Duration::from_secs(120)));
//! assert_eq!(effective_deadline(asked, Duration::from_secs(30)), Duration::from_secs(30));
//! ```

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::{Either, select};
use http::HeaderMap;
use tokio::sync::Notify;

use crate::handler::CallOutcome;
use crate::wire::RpcFailure;

/// The request header naming the client's deadline in milliseconds.
pub const DEADLINE_HEADER: &str = "x-rpc-deadline-ms";

/// The server's default cap on any call's deadline.
pub const DEFAULT_MAX_DEADLINE: Duration = Duration::from_secs(30);

/// The deadline the client asked for: `x-rpc-deadline-ms` as a whole number of milliseconds
/// (anything else is ignored, so the server maximum applies).
pub fn requested_deadline(headers: &HeaderMap) -> Option<Duration> {
  let text = headers.get(DEADLINE_HEADER)?.to_str().ok()?;
  text.trim().parse::<u64>().ok().map(Duration::from_millis)
}

/// The deadline a call runs under: the requested one, never longer than `max`.
pub fn effective_deadline(requested: Option<Duration>, max: Duration) -> Duration {
  requested.map_or(max, |asked| asked.min(max))
}

/// Cancels the calls holding the matching [`CancelSignal`]s (a `cancel` frame).
#[derive(Clone, Debug, Default)]
pub struct CancelHandle(Arc<Notify>);

/// Resolves once its [`CancelHandle`] cancelled.
#[derive(Clone, Debug)]
pub struct CancelSignal(Arc<Notify>);

/// A connected handle and signal.
pub fn cancel_pair() -> (CancelHandle, CancelSignal) {
  let notify = Arc::new(Notify::new());
  (CancelHandle(notify.clone()), CancelSignal(notify))
}

impl CancelHandle {
  /// Cancel: the call answers `Cancelled` and its handler fiber is interrupted.
  pub fn cancel(&self) {
    // `notify_one` stores a permit when nobody waits yet, so an early cancel is not lost.
    self.0.notify_one();
  }
}

impl CancelSignal {
  /// Wait for the cancel.
  pub async fn cancelled(&self) {
    self.0.notified().await;
  }
}

/// The deadline and cancel signal of one call.
#[derive(Clone, Debug, Default)]
pub struct CallControl {
  /// After this long the call answers `DeadlineExceeded` (`None`: no deadline).
  pub deadline: Option<Duration>,
  /// When it fires the call answers `Cancelled`.
  pub cancel: Option<CancelSignal>,
}

impl CallControl {
  /// No deadline, no cancel signal.
  pub fn none() -> Self {
    Self::default()
  }

  /// Run under `deadline`.
  #[must_use]
  pub fn with_deadline(mut self, deadline: Duration) -> Self {
    self.deadline = Some(deadline);
    self
  }

  /// Cancel on `signal`.
  #[must_use]
  pub fn with_cancel(mut self, signal: CancelSignal) -> Self {
    self.cancel = Some(signal);
    self
  }
}

/// Run `call` under `control`: its outcome, or `DeadlineExceeded` / `Cancelled` — in which case
/// `call` is dropped before this returns (its fiber interrupted, its finalizers run).
pub async fn controlled<F>(call: F, control: CallControl) -> CallOutcome
where
  F: Future<Output = CallOutcome> + Send,
{
  let CallControl { deadline, cancel } = control;
  let stop = async move {
    let timer = async {
      match deadline {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
      }
    };
    let cancelled = async {
      match &cancel {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
      }
    };
    match select(Box::pin(timer), Box::pin(cancelled)).await {
      Either::Left(_) => RpcFailure::DeadlineExceeded,
      Either::Right(_) => RpcFailure::Cancelled,
    }
  };
  match select(Box::pin(call), Box::pin(stop)).await {
    Either::Left((outcome, _)) => outcome,
    Either::Right((failure, call)) => {
      drop(call);
      CallOutcome::failure(failure)
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::wire::WireExit;

  #[test]
  fn deadlines_parse_and_clamp() {
    let mut headers = HeaderMap::new();
    assert_eq!(requested_deadline(&headers), None);
    headers.insert(DEADLINE_HEADER, " 250 ".parse().unwrap());
    assert_eq!(
      requested_deadline(&headers),
      Some(Duration::from_millis(250))
    );
    headers.insert(DEADLINE_HEADER, "soon".parse().unwrap());
    assert_eq!(requested_deadline(&headers), None);
    let max = Duration::from_secs(1);
    assert_eq!(effective_deadline(None, max), max);
    assert_eq!(
      effective_deadline(Some(Duration::from_millis(5)), max),
      Duration::from_millis(5)
    );
    let control = CallControl::none().with_deadline(max);
    assert_eq!(control.deadline, Some(max));
    assert!(format!("{control:?}").contains("deadline"));
  }

  #[tokio::test]
  async fn controlled_calls_finish_expire_or_cancel() {
    let done = controlled(
      async { CallOutcome::success(serde_json::json!(1)) },
      CallControl::none(),
    )
    .await;
    assert!(done.exit.is_success());

    let slow = async {
      tokio::time::sleep(Duration::from_secs(60)).await;
      CallOutcome::success(serde_json::Value::Null)
    };
    let expired = controlled(
      slow,
      CallControl::none().with_deadline(Duration::from_millis(5)),
    )
    .await;
    assert_eq!(expired.status, http::StatusCode::GATEWAY_TIMEOUT);

    let (handle, signal) = cancel_pair();
    handle.cancel();
    let cancelled = controlled(
      std::future::pending::<CallOutcome>(),
      CallControl::none().with_cancel(signal),
    )
    .await;
    let WireExit::Failure { error } = cancelled.exit else {
      panic!("expected a failure");
    };
    assert_eq!(error["_tag"], "Cancelled");
    assert!(format!("{:?}", CancelHandle::default()).contains("CancelHandle"));
  }
}
