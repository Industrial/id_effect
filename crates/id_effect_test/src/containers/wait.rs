//! [`Wait`]: when a started container counts as ready, and the waits themselves (run on the
//! Docker runtime, under the spec's readiness timeout).

use std::time::Duration;

use bollard::models::HealthStatusEnum;
use testcontainers::core::client::docker_client_instance;
use testcontainers::{ContainerAsync, GenericImage};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// How often [`Wait::Healthy`] inspects the container.
const HEALTH_POLL: Duration = Duration::from_millis(100);

/// When the container counts as ready. Strategies run in order; together they are bounded by
/// the spec's [`ready_timeout`](super::ContainerSpec::ready_timeout).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Wait {
  /// A line of standard output contains this message.
  Stdout(String),
  /// A line of standard error contains this message.
  Stderr(String),
  /// A line of either stream contains this message.
  Log(String),
  /// The container's health check reports healthy: the spec's
  /// [`health_cmd`](super::ContainerSpec::health_cmd), or the image's own `HEALTHCHECK`.
  Healthy,
  /// A fixed delay.
  Delay(Duration),
}

impl Wait {
  /// [`Wait::Stdout`].
  pub fn stdout(message: impl Into<String>) -> Self {
    Self::Stdout(message.into())
  }

  /// [`Wait::Stderr`].
  pub fn stderr(message: impl Into<String>) -> Self {
    Self::Stderr(message.into())
  }

  /// [`Wait::Log`].
  pub fn log(message: impl Into<String>) -> Self {
    Self::Log(message.into())
  }

  /// [`Wait::Healthy`].
  pub fn healthy() -> Self {
    Self::Healthy
  }

  /// [`Wait::Delay`].
  pub fn delay(delay: Duration) -> Self {
    Self::Delay(delay)
  }
}

/// Why a container will not become ready.
pub(crate) type NotReady = Box<dyn std::error::Error + Send + Sync>;

/// Runs `waits` in order against `container`; the error says why it will not become ready.
pub(crate) async fn ready(
  container: &ContainerAsync<GenericImage>,
  waits: &[Wait],
) -> Result<(), NotReady> {
  for wait in waits {
    match wait {
      Wait::Stdout(message) => log_line(container.stdout(true), message).await?,
      Wait::Stderr(message) => log_line(container.stderr(true), message).await?,
      Wait::Log(message) => {
        either_log_line(container.stdout(true), container.stderr(true), message).await?;
      }
      Wait::Healthy => healthy(container.id()).await?,
      Wait::Delay(delay) => tokio::time::sleep(*delay).await,
    }
  }
  Ok(())
}

/// Reads `log` until a line contains `message`; fails when the log ends first (the container
/// stopped).
async fn log_line<L: AsyncBufRead + Unpin>(mut log: L, message: &str) -> Result<(), NotReady> {
  let mut line = Vec::new();
  loop {
    line.clear();
    if log.read_until(b'\n', &mut line).await? == 0 {
      return Err(format!("its log ended before a line containing {message:?}").into());
    }
    if String::from_utf8_lossy(&line).contains(message) {
      return Ok(());
    }
  }
}

/// [`log_line`] over two logs at once: ready when either has the line, failed when both end.
async fn either_log_line<L>(first: L, second: L, message: &str) -> Result<(), NotReady>
where
  L: AsyncBufRead + Unpin + Send + 'static,
{
  let (tx, mut rx) = tokio::sync::mpsc::channel(2);
  let readers = [first, second].map(|log| {
    let (tx, message) = (tx.clone(), message.to_owned());
    tokio::spawn(async move {
      let _ = tx.send(log_line(log, &message).await).await;
    })
  });
  drop(tx);
  let mut outcome = Ok(());
  while let Some(result) = rx.recv().await {
    outcome = result;
    if outcome.is_ok() {
      break;
    }
  }
  for reader in readers {
    reader.abort();
  }
  outcome
}

/// Polls the container until its health check reports healthy; fails when it reports
/// unhealthy, when the container exits, or when it has no health check.
async fn healthy(id: &str) -> Result<(), NotReady> {
  let docker = docker_client_instance().await?;
  loop {
    let state = docker
      .inspect_container(id, None)
      .await?
      .state
      .unwrap_or_default();
    if state.running == Some(false) {
      let code = state.exit_code.unwrap_or_default();
      return Err(format!("it exited (code {code}) before its health check passed").into());
    }
    match state.health.and_then(|health| health.status) {
      Some(HealthStatusEnum::HEALTHY) => return Ok(()),
      Some(HealthStatusEnum::UNHEALTHY) => return Err("its health check reports unhealthy".into()),
      Some(HealthStatusEnum::STARTING) => tokio::time::sleep(HEALTH_POLL).await,
      _ => return Err("it has no health check (set ContainerSpec::health_cmd)".into()),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn constructors_build_each_strategy() {
    assert_eq!(Wait::stdout("a"), Wait::Stdout("a".into()));
    assert_eq!(Wait::stderr("b"), Wait::Stderr("b".into()));
    assert_eq!(Wait::log("c"), Wait::Log("c".into()));
    assert_eq!(Wait::healthy(), Wait::Healthy);
    assert_eq!(Wait::delay(Duration::ZERO), Wait::Delay(Duration::ZERO));
  }

  #[tokio::test]
  async fn a_log_line_is_found_or_the_log_ends_first() {
    let log: &[u8] = b"booting\nready to serve\n";
    assert!(log_line(log, "ready").await.is_ok());
    let ended = log_line(log, "never").await.unwrap_err();
    assert_eq!(
      ended.to_string(),
      r#"its log ended before a line containing "never""#
    );
  }

  #[tokio::test]
  async fn either_log_is_enough_and_both_must_end_to_fail() {
    let quiet: &'static [u8] = b"nothing here\n";
    let ready: &'static [u8] = b"Ready\n";
    assert!(either_log_line(quiet, ready, "Ready").await.is_ok());
    assert!(either_log_line(ready, quiet, "Ready").await.is_ok());
    assert!(either_log_line(quiet, quiet, "Ready").await.is_err());
  }
}
