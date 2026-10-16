//! [`Vault`]: a HashiCorp Vault dev server ([`VaultContainer`]).

use std::time::Duration;

use super::{Container, ContainerImage, ContainerSpec, DEFAULT_READY_TIMEOUT, Wait};

/// Vault's container port.
pub const VAULT_PORT: u16 = 8200;

/// A Vault server in dev mode (`hashicorp/vault`, default tag `1.20`, root token `root-token`,
/// KV v2 mounted at `secret/`, in memory, unsealed). Ready once it logs its dev-mode warning.
#[derive(Clone, Debug)]
pub struct Vault {
  tag: String,
  root_token: String,
  ready_timeout: Duration,
}

impl Default for Vault {
  fn default() -> Self {
    Self::new()
  }
}

impl Vault {
  /// `hashicorp/vault:1.20`, root token `root-token`.
  pub fn new() -> Self {
    Self {
      tag: "1.20".into(),
      root_token: "root-token".into(),
      ready_timeout: DEFAULT_READY_TIMEOUT,
    }
  }

  /// The image tag.
  pub fn tag(mut self, tag: impl Into<String>) -> Self {
    self.tag = tag.into();
    self
  }

  /// The dev server's root token (`VAULT_DEV_ROOT_TOKEN_ID`).
  pub fn root_token(mut self, token: impl Into<String>) -> Self {
    self.root_token = token.into();
    self
  }

  /// Bounds the readiness wait ([`ContainerSpec::ready_timeout`]).
  pub fn ready_timeout(mut self, timeout: Duration) -> Self {
    self.ready_timeout = timeout;
    self
  }
}

impl ContainerImage for Vault {
  type Running = VaultContainer;

  fn spec(&self) -> ContainerSpec {
    ContainerSpec::new("hashicorp/vault", &self.tag)
      .port(VAULT_PORT)
      .env("VAULT_DEV_ROOT_TOKEN_ID", &self.root_token)
      .env("SKIP_SETCAP", "true")
      .wait(Wait::stdout("Development mode should NOT be used"))
      .ready_timeout(self.ready_timeout)
  }

  fn running(&self, container: Container) -> VaultContainer {
    VaultContainer {
      port: container.mapped_port(VAULT_PORT),
      container,
      root_token: self.root_token.clone(),
    }
  }

  fn container(running: &VaultContainer) -> &Container {
    &running.container
  }
}

/// A started, ready Vault dev server: the capability [`Vault::layer`](ContainerImage::layer)
/// provides.
#[derive(Clone, Debug)]
pub struct VaultContainer {
  container: Container,
  port: u16,
  root_token: String,
}

impl VaultContainer {
  /// `http://host:port` (`VAULT_ADDR`).
  pub fn addr(&self) -> String {
    format!("http://{}:{}", self.container.host(), self.port)
  }

  /// The root token.
  pub fn root_token(&self) -> &str {
    &self.root_token
  }

  /// The host port of [`VAULT_PORT`].
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
    assert_eq!(Vault::default().spec().image(), "hashicorp/vault:1.20");
    let spec = Vault::new()
      .tag("1.19")
      .ready_timeout(Duration::from_secs(9))
      .spec();
    assert_eq!(spec.image(), "hashicorp/vault:1.19");
    assert_eq!(spec.ports(), [VAULT_PORT]);
  }
}
