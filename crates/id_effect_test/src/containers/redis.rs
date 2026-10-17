//! [`Redis`]: a Redis server ([`RedisContainer`]).

use std::time::Duration;

use super::{Container, ContainerImage, ContainerSpec, DEFAULT_READY_TIMEOUT, Wait};

/// Redis's container port.
pub const REDIS_PORT: u16 = 6379;

/// A Redis server (official `redis` image, default tag `7-alpine`, no password). Ready once it
/// logs "Ready to accept connections".
#[derive(Clone, Debug)]
pub struct Redis {
  tag: String,
  ready_timeout: Duration,
}

impl Default for Redis {
  fn default() -> Self {
    Self::new()
  }
}

impl Redis {
  /// `redis:7-alpine`.
  pub fn new() -> Self {
    Self {
      tag: "7-alpine".into(),
      ready_timeout: DEFAULT_READY_TIMEOUT,
    }
  }

  /// The image tag.
  pub fn tag(mut self, tag: impl Into<String>) -> Self {
    self.tag = tag.into();
    self
  }

  /// Bounds the readiness wait ([`ContainerSpec::ready_timeout`]).
  pub fn ready_timeout(mut self, timeout: Duration) -> Self {
    self.ready_timeout = timeout;
    self
  }
}

impl ContainerImage for Redis {
  type Running = RedisContainer;

  fn spec(&self) -> ContainerSpec {
    ContainerSpec::new("redis", &self.tag)
      .port(REDIS_PORT)
      .wait(Wait::stdout("Ready to accept connections"))
      .ready_timeout(self.ready_timeout)
  }

  fn running(&self, container: Container) -> RedisContainer {
    RedisContainer {
      port: container.mapped_port(REDIS_PORT),
      container,
    }
  }

  fn container(running: &RedisContainer) -> &Container {
    &running.container
  }
}

/// A started, ready Redis server: the capability [`Redis::layer`](ContainerImage::layer)
/// provides.
#[derive(Clone, Debug)]
pub struct RedisContainer {
  container: Container,
  port: u16,
}

impl RedisContainer {
  /// `redis://host:port`.
  pub fn url(&self) -> String {
    format!("redis://{}:{}", self.container.host(), self.port)
  }

  /// The host.
  pub fn host(&self) -> &str {
    self.container.host()
  }

  /// The host port of [`REDIS_PORT`].
  pub fn port(&self) -> u16 {
    self.port
  }

  /// The container.
  pub fn container(&self) -> &Container {
    &self.container
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_builder_shapes_the_spec() {
    assert_eq!(Redis::default().spec().image(), "redis:7-alpine");
    let spec = Redis::new()
      .tag("8-alpine")
      .ready_timeout(Duration::from_secs(9))
      .spec();
    assert_eq!(spec.image(), "redis:8-alpine");
    assert_eq!(spec.ports(), [REDIS_PORT]);
  }
}
