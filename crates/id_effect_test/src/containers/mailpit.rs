//! [`Mailpit`]: an SMTP server that keeps every message it receives and serves them over an
//! HTTP API ([`MailpitContainer`]).
//!
//! The image is `axllent/mailpit` (MIT), pinned by digest like the other new images of the 0.10
//! release (D11). Mail tests send through a real SMTP client to [`MAILPIT_SMTP_PORT`] and read
//! what arrived from [`MAILPIT_API_PORT`] (`GET /api/v1/messages`, `GET /api/v1/message/{id}`).

use std::time::Duration;

use super::{Container, ContainerImage, ContainerSpec, DEFAULT_READY_TIMEOUT, Wait};

/// The SMTP port (no TLS, no authentication).
pub const MAILPIT_SMTP_PORT: u16 = 1025;

/// The web UI and HTTP API port.
pub const MAILPIT_API_PORT: u16 = 8025;

/// The image name.
pub const MAILPIT_IMAGE: &str = "axllent/mailpit";

/// The default tag: Mailpit 1.31.6, pinned to the digest of its multi-arch index (pulled
/// 2026-10-17).
pub const MAILPIT_TAG: &str =
  "v1.31.6@sha256:23fe39e81bde596a918000b3135c0f85dc8bb4c9ea6be274b051143f3e300387";

/// A Mailpit server (`axllent/mailpit` at [`MAILPIT_TAG`]): accepts any message on
/// [`MAILPIT_SMTP_PORT`] and lists it on the API at [`MAILPIT_API_PORT`]. Ready once the HTTP
/// server logs that it is accessible (the SMTP listener starts before it).
#[derive(Clone, Debug)]
pub struct Mailpit {
  tag: String,
  ready_timeout: Duration,
}

impl Default for Mailpit {
  fn default() -> Self {
    Self::new()
  }
}

impl Mailpit {
  /// `axllent/mailpit` at [`MAILPIT_TAG`].
  pub fn new() -> Self {
    Self {
      tag: MAILPIT_TAG.into(),
      ready_timeout: DEFAULT_READY_TIMEOUT,
    }
  }

  /// The image tag (a Mailpit release, optionally `tag@sha256:digest`).
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

impl ContainerImage for Mailpit {
  type Running = MailpitContainer;

  fn spec(&self) -> ContainerSpec {
    ContainerSpec::new(MAILPIT_IMAGE, &self.tag)
      .port(MAILPIT_SMTP_PORT)
      .port(MAILPIT_API_PORT)
      .wait(Wait::stdout("[http] accessible via"))
      .ready_timeout(self.ready_timeout)
  }

  fn running(&self, container: Container) -> MailpitContainer {
    MailpitContainer {
      smtp_port: container.mapped_port(MAILPIT_SMTP_PORT),
      api_port: container.mapped_port(MAILPIT_API_PORT),
      container,
    }
  }

  fn container(running: &MailpitContainer) -> &Container {
    &running.container
  }
}

/// A started, ready Mailpit server: the capability [`Mailpit::layer`](ContainerImage::layer)
/// provides.
#[derive(Clone, Debug)]
pub struct MailpitContainer {
  container: Container,
  smtp_port: u16,
  api_port: u16,
}

impl MailpitContainer {
  /// The host.
  pub fn host(&self) -> &str {
    self.container.host()
  }

  /// The host port of [`MAILPIT_SMTP_PORT`].
  pub fn smtp_port(&self) -> u16 {
    self.smtp_port
  }

  /// The host port of [`MAILPIT_API_PORT`].
  pub fn api_port(&self) -> u16 {
    self.api_port
  }

  /// `http://host:port`, the base URL of the web UI and the API (`/api/v1/...`).
  pub fn api_url(&self) -> String {
    format!("http://{}:{}", self.container.host(), self.api_port)
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
    assert_eq!(
      Mailpit::default().spec().image(),
      format!("axllent/mailpit:{MAILPIT_TAG}")
    );
    assert!(MAILPIT_TAG.contains("@sha256:"), "pinned by digest (D11)");
    let spec = Mailpit::new()
      .tag("v1.31.5")
      .ready_timeout(Duration::from_secs(9))
      .spec();
    assert_eq!(spec.image(), "axllent/mailpit:v1.31.5");
    assert_eq!(spec.ports(), [MAILPIT_SMTP_PORT, MAILPIT_API_PORT]);
  }
}
