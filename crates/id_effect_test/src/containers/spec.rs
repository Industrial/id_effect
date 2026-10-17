//! [`ContainerSpec`]: the generic image builder, and [`start`], which starts one and waits until
//! it is ready.

use std::time::Duration;

use testcontainers::core::{Healthcheck, IntoContainerPort, Mount};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerRequest, GenericImage, ImageExt};

use super::docker::{SESSION, SESSION_LABEL, ping};
use super::wait::ready;
use super::{Container, ContainerError, Wait};

/// The readiness timeout of a [`ContainerSpec`] and of the built-in images unless changed.
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a [`health_cmd`](ContainerSpec::health_cmd) runs.
const HEALTH_INTERVAL: Duration = Duration::from_millis(250);

/// Bounds creating and starting the container (pulling a missing image is not included).
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// A container to start: image, environment, published ports, command, readiness. Every
/// container port added with [`port`](Self::port) is published on a random host port, read back
/// with [`Container::host_port`].
///
/// A spec is itself a [`ContainerImage`](super::ContainerImage) whose running value is the
/// [`Container`], so `spec.layer()`, `spec.acquire(&scope)` and `Shared::new(|| spec)` work as
/// for the built-in images.
///
/// ```
/// use std::time::Duration;
/// use id_effect_test::{ContainerSpec, Wait};
///
/// let echo = ContainerSpec::new("hashicorp/http-echo", "1.0")
///   .cmd(["-text=hello"])
///   .port(5678)
///   .wait(Wait::stderr("server is listening"))
///   .ready_timeout(Duration::from_secs(30));
/// assert_eq!(echo.image(), "hashicorp/http-echo:1.0");
/// assert_eq!(echo.ports(), [5678]);
/// ```
#[derive(Clone, Debug)]
pub struct ContainerSpec {
  name: String,
  tag: String,
  env: Vec<(String, String)>,
  ports: Vec<u16>,
  cmd: Vec<String>,
  waits: Vec<Wait>,
  health_cmd: Option<Vec<String>>,
  tmpfs: Vec<String>,
  labels: Vec<(String, String)>,
  files: Vec<(String, Vec<u8>)>,
  ready_timeout: Duration,
}

impl ContainerSpec {
  /// The image `name:tag` (`name` may include a registry, e.g. `quay.io/keycloak/keycloak`),
  /// with nothing published and no readiness wait.
  pub fn new(name: impl Into<String>, tag: impl Into<String>) -> Self {
    Self {
      name: name.into(),
      tag: tag.into(),
      env: Vec::new(),
      ports: Vec::new(),
      cmd: Vec::new(),
      waits: Vec::new(),
      health_cmd: None,
      tmpfs: Vec::new(),
      labels: Vec::new(),
      files: Vec::new(),
      ready_timeout: DEFAULT_READY_TIMEOUT,
    }
  }

  /// `name:tag`.
  pub fn image(&self) -> String {
    format!("{}:{}", self.name, self.tag)
  }

  /// The container ports published by [`port`](Self::port), in order.
  pub fn ports(&self) -> &[u16] {
    &self.ports
  }

  /// Sets the environment variable `name` in the container.
  pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
    self.env.push((name.into(), value.into()));
    self
  }

  /// Publishes the TCP container port `port` on a random host port.
  pub fn port(mut self, port: u16) -> Self {
    self.ports.push(port);
    self
  }

  /// Replaces the image's command (its `CMD`; the entrypoint stays).
  pub fn cmd(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
    self.cmd = args.into_iter().map(Into::into).collect();
    self
  }

  /// Adds a readiness strategy (they run in the order added).
  pub fn wait(mut self, wait: Wait) -> Self {
    self.waits.push(wait);
    self
  }

  /// A health check command run inside the container every 250 ms, for [`Wait::Healthy`].
  pub fn health_cmd(mut self, cmd: impl IntoIterator<Item = impl Into<String>>) -> Self {
    self.health_cmd = Some(cmd.into_iter().map(Into::into).collect());
    self
  }

  /// Mounts an in-memory filesystem at `path` (a database's data directory: faster, and no
  /// volume left behind).
  pub fn tmpfs(mut self, path: impl Into<String>) -> Self {
    self.tmpfs.push(path.into());
    self
  }

  /// Adds a Docker label.
  pub fn label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
    self.labels.push((key.into(), value.into()));
    self
  }

  /// Copies `contents` into the container at the absolute `path` before it starts.
  pub fn file(mut self, path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
    self.files.push((path.into(), contents.into()));
    self
  }

  /// Bounds the readiness wait (default [`DEFAULT_READY_TIMEOUT`]). When it passes, the start
  /// fails with [`ContainerError::NotReady`] and the container is removed. Pulling a missing
  /// image happens before and is not bounded by it.
  pub fn ready_timeout(mut self, timeout: Duration) -> Self {
    self.ready_timeout = timeout;
    self
  }

  /// The testcontainers request: everything but the readiness wait, which [`start`] runs.
  fn request(&self) -> ContainerRequest<GenericImage> {
    let image = self
      .ports
      .iter()
      .fold(GenericImage::new(&self.name, &self.tag), |image, port| {
        image.with_exposed_port(port.tcp())
      });
    let mut request = image
      .with_label(SESSION_LABEL, SESSION.as_str())
      .with_cmd(self.cmd.clone())
      .with_startup_timeout(START_TIMEOUT);
    for (name, value) in &self.env {
      request = request.with_env_var(name, value);
    }
    if let Some(cmd) = &self.health_cmd {
      request = request.with_health_check(
        Healthcheck::cmd(cmd.clone())
          .with_interval(HEALTH_INTERVAL)
          .with_timeout(Duration::from_secs(5))
          .with_retries(u32::MAX),
      );
    }
    for path in &self.tmpfs {
      request = request.with_mount(Mount::tmpfs_mount(path));
    }
    for (key, value) in &self.labels {
      request = request.with_label(key, value);
    }
    for (path, contents) in &self.files {
      request = request.with_copy_to(path.clone(), contents.clone());
    }
    request
  }
}

/// Starts `spec` (on the Docker runtime): creates and starts the container, then runs its
/// readiness strategies under its readiness timeout. A container that does not become ready is
/// removed before the error is returned.
pub(crate) async fn start(spec: ContainerSpec) -> Result<Container, ContainerError> {
  ping().await?;
  let image = spec.image();
  let started = spec
    .request()
    .start()
    .await
    .map_err(|e| ContainerError::Start {
      image: image.clone(),
      reason: e.to_string(),
    })?;
  let id = started.id().to_owned();
  let failed = match tokio::time::timeout(spec.ready_timeout, ready(&started, &spec.waits)).await {
    Ok(Ok(())) => None,
    Ok(Err(reason)) => Some(ContainerError::Unready {
      image: image.clone(),
      id,
      reason: reason.to_string(),
    }),
    Err(_) => Some(ContainerError::NotReady {
      image: image.clone(),
      id,
      timeout: spec.ready_timeout,
    }),
  };
  if let Some(error) = failed {
    let _ = started.rm().await;
    return Err(error);
  }
  Container::resolve(started, image, &spec.ports).await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn builder_records_every_setting() {
    let spec = ContainerSpec::new("redis", "7-alpine")
      .env("A", "1")
      .port(6379)
      .port(6380)
      .cmd(["redis-server", "--save", ""])
      .wait(Wait::stdout("Ready"))
      .health_cmd(["redis-cli", "ping"])
      .tmpfs("/data")
      .label("team", "a")
      .file("/etc/motd", "hi")
      .ready_timeout(Duration::from_secs(5));
    assert_eq!(spec.image(), "redis:7-alpine");
    assert_eq!(spec.ports(), [6379, 6380]);
    assert_eq!(spec.cmd, ["redis-server", "--save", ""]);
    assert_eq!(spec.env, [("A".to_owned(), "1".to_owned())]);
    assert_eq!(spec.waits, [Wait::stdout("Ready")]);
    assert_eq!(spec.ready_timeout, Duration::from_secs(5));
    let request = spec.request();
    assert_eq!(request.startup_timeout(), Some(START_TIMEOUT));
    assert!(request.health_check().is_some());
    assert_eq!(request.mounts().count(), 1);
    assert_eq!(request.copy_to_sources().count(), 1);
    assert_eq!(request.labels().get(SESSION_LABEL), Some(&*SESSION));
    assert_eq!(request.labels().get("team").map(String::as_str), Some("a"));
    assert_eq!(request.cmd().count(), 3);
    assert_eq!(request.env_vars().count(), 1);
  }
}
