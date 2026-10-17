//! [`Keycloak`]: an OpenID Connect provider ([`KeycloakContainer`]) for login tests.

use std::time::Duration;

use super::{Container, ContainerImage, ContainerSpec, Wait};

/// Keycloak's HTTP container port.
pub const KEYCLOAK_PORT: u16 = 8080;

/// The readiness timeout of [`Keycloak`] (a JVM start, plus realm imports).
pub const KEYCLOAK_READY_TIMEOUT: Duration = Duration::from_secs(180);

/// A Keycloak server in development mode (`quay.io/keycloak/keycloak`, default tag `26.4`,
/// admin `admin`/`admin` in the `master` realm), with optional realm imports. Ready once it
/// logs that it is listening.
#[derive(Clone, Debug)]
pub struct Keycloak {
  tag: String,
  admin_user: String,
  admin_password: String,
  realms: Vec<Vec<u8>>,
  ready_timeout: Duration,
}

impl Default for Keycloak {
  fn default() -> Self {
    Self::new()
  }
}

impl Keycloak {
  /// `quay.io/keycloak/keycloak:26.4`, admin `admin`/`admin`.
  pub fn new() -> Self {
    Self {
      tag: "26.4".into(),
      admin_user: "admin".into(),
      admin_password: "admin".into(),
      realms: Vec::new(),
      ready_timeout: KEYCLOAK_READY_TIMEOUT,
    }
  }

  /// The image tag.
  pub fn tag(mut self, tag: impl Into<String>) -> Self {
    self.tag = tag.into();
    self
  }

  /// The bootstrap admin of the `master` realm.
  pub fn admin(mut self, user: impl Into<String>, password: impl Into<String>) -> Self {
    self.admin_user = user.into();
    self.admin_password = password.into();
    self
  }

  /// Imports a realm export (JSON: realm, clients, users) at start (`--import-realm`).
  pub fn import_realm(mut self, realm_json: impl Into<Vec<u8>>) -> Self {
    self.realms.push(realm_json.into());
    self
  }

  /// Bounds the readiness wait ([`ContainerSpec::ready_timeout`]).
  pub fn ready_timeout(mut self, timeout: Duration) -> Self {
    self.ready_timeout = timeout;
    self
  }
}

impl ContainerImage for Keycloak {
  type Running = KeycloakContainer;

  fn spec(&self) -> ContainerSpec {
    let import = (!self.realms.is_empty()).then_some("--import-realm");
    let spec = ContainerSpec::new("quay.io/keycloak/keycloak", &self.tag)
      .port(KEYCLOAK_PORT)
      .env("KC_BOOTSTRAP_ADMIN_USERNAME", &self.admin_user)
      .env("KC_BOOTSTRAP_ADMIN_PASSWORD", &self.admin_password)
      .cmd(["start-dev"].into_iter().chain(import))
      .wait(Wait::log("Listening on:"))
      .ready_timeout(self.ready_timeout);
    self
      .realms
      .iter()
      .enumerate()
      .fold(spec, |spec, (i, realm)| {
        spec.file(
          format!("/opt/keycloak/data/import/realm-{i}.json"),
          realm.clone(),
        )
      })
  }

  fn running(&self, container: Container) -> KeycloakContainer {
    KeycloakContainer {
      port: container.mapped_port(KEYCLOAK_PORT),
      container,
      admin_user: self.admin_user.clone(),
      admin_password: self.admin_password.clone(),
    }
  }

  fn container(running: &KeycloakContainer) -> &Container {
    &running.container
  }
}

/// A started, ready Keycloak server: the capability [`Keycloak::layer`](ContainerImage::layer)
/// provides.
#[derive(Clone, Debug)]
pub struct KeycloakContainer {
  container: Container,
  port: u16,
  admin_user: String,
  admin_password: String,
}

impl KeycloakContainer {
  /// `http://host:port`.
  pub fn base_url(&self) -> String {
    format!("http://{}:{}", self.container.host(), self.port)
  }

  /// The OpenID Connect issuer of `realm`: `http://host:port/realms/<realm>` (discovery at
  /// `<issuer>/.well-known/openid-configuration`).
  pub fn issuer(&self, realm: &str) -> String {
    format!("{}/realms/{realm}", self.base_url())
  }

  /// The `master` realm's admin user.
  pub fn admin_user(&self) -> &str {
    &self.admin_user
  }

  /// Its password.
  pub fn admin_password(&self) -> &str {
    &self.admin_password
  }

  /// The host port of [`KEYCLOAK_PORT`].
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
    assert_eq!(
      Keycloak::default().spec().image(),
      "quay.io/keycloak/keycloak:26.4"
    );
    let spec = Keycloak::new()
      .tag("26.3")
      .ready_timeout(Duration::from_secs(9))
      .spec();
    assert_eq!(spec.image(), "quay.io/keycloak/keycloak:26.3");
    assert_eq!(spec.ports(), [KEYCLOAK_PORT]);
  }
}
