//! [`MySql`]: a MySQL server ([`MySqlContainer`]).

use std::time::Duration;

use super::{Container, ContainerImage, ContainerSpec, DEFAULT_READY_TIMEOUT, Wait};

/// MySQL's container port.
pub const MYSQL_PORT: u16 = 3306;

/// A MySQL server (official `mysql` image, default tag `8.4`; user, password and database
/// `test`, root password `root`). Its data directory is a tmpfs. Ready once `mysqladmin ping`
/// answers over TCP (the image's init runs with networking off, so this is after init).
#[derive(Clone, Debug)]
pub struct MySql {
  tag: String,
  user: String,
  password: String,
  database: String,
  root_password: String,
  ready_timeout: Duration,
}

impl Default for MySql {
  fn default() -> Self {
    Self::new()
  }
}

impl MySql {
  /// `mysql:8.4`, user/password/database `test`, root password `root`.
  pub fn new() -> Self {
    Self {
      tag: "8.4".into(),
      user: "test".into(),
      password: "test".into(),
      database: "test".into(),
      root_password: "root".into(),
      ready_timeout: DEFAULT_READY_TIMEOUT,
    }
  }

  /// The image tag.
  pub fn tag(mut self, tag: impl Into<String>) -> Self {
    self.tag = tag.into();
    self
  }

  /// The user created at start (`MYSQL_USER`; not `root`), granted all on the database.
  pub fn user(mut self, user: impl Into<String>) -> Self {
    self.user = user.into();
    self
  }

  /// Its password (`MYSQL_PASSWORD`).
  pub fn password(mut self, password: impl Into<String>) -> Self {
    self.password = password.into();
    self
  }

  /// The database created at start (`MYSQL_DATABASE`).
  pub fn database(mut self, database: impl Into<String>) -> Self {
    self.database = database.into();
    self
  }

  /// The root password (`MYSQL_ROOT_PASSWORD`).
  pub fn root_password(mut self, password: impl Into<String>) -> Self {
    self.root_password = password.into();
    self
  }

  /// Bounds the readiness wait ([`ContainerSpec::ready_timeout`]).
  pub fn ready_timeout(mut self, timeout: Duration) -> Self {
    self.ready_timeout = timeout;
    self
  }
}

impl ContainerImage for MySql {
  type Running = MySqlContainer;

  fn spec(&self) -> ContainerSpec {
    ContainerSpec::new("mysql", &self.tag)
      .port(MYSQL_PORT)
      .env("MYSQL_ROOT_PASSWORD", &self.root_password)
      .env("MYSQL_DATABASE", &self.database)
      .env("MYSQL_USER", &self.user)
      .env("MYSQL_PASSWORD", &self.password)
      .tmpfs("/var/lib/mysql")
      .health_cmd([
        "mysqladmin".to_owned(),
        "ping".into(),
        "-h127.0.0.1".into(),
        "-uroot".into(),
        format!("-p{}", self.root_password),
        "--silent".into(),
      ])
      .wait(Wait::healthy())
      .ready_timeout(self.ready_timeout)
  }

  fn running(&self, container: Container) -> MySqlContainer {
    MySqlContainer {
      port: container.mapped_port(MYSQL_PORT),
      container,
      user: self.user.clone(),
      password: self.password.clone(),
      database: self.database.clone(),
      root_password: self.root_password.clone(),
    }
  }

  fn container(running: &MySqlContainer) -> &Container {
    &running.container
  }
}

/// A started, ready MySQL server: the capability [`MySql::layer`](ContainerImage::layer)
/// provides.
#[derive(Clone, Debug)]
pub struct MySqlContainer {
  container: Container,
  port: u16,
  user: String,
  password: String,
  database: String,
  root_password: String,
}

impl MySqlContainer {
  /// `mysql://user:password@host:port/database`.
  pub fn url(&self) -> String {
    self.url_for(&self.database)
  }

  /// The URL of another database on the same server, as the same user.
  pub fn url_for(&self, database: &str) -> String {
    format!(
      "mysql://{}:{}@{}:{}/{database}",
      self.user,
      self.password,
      self.container.host(),
      self.port
    )
  }

  /// `mysql://root:<root password>@host:port/database` (creating databases, granting).
  pub fn root_url(&self) -> String {
    format!(
      "mysql://root:{}@{}:{}/{}",
      self.root_password,
      self.container.host(),
      self.port,
      self.database
    )
  }

  /// The host.
  pub fn host(&self) -> &str {
    self.container.host()
  }

  /// The host port of [`MYSQL_PORT`].
  pub fn port(&self) -> u16 {
    self.port
  }

  /// The user.
  pub fn user(&self) -> &str {
    &self.user
  }

  /// Its password.
  pub fn password(&self) -> &str {
    &self.password
  }

  /// The database created at start.
  pub fn database(&self) -> &str {
    &self.database
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
    assert_eq!(MySql::default().spec().image(), "mysql:8.4");
    let spec = MySql::new()
      .tag("8.0")
      .user("ledger")
      .password("pw")
      .database("books")
      .root_password("r")
      .ready_timeout(Duration::from_secs(9))
      .spec();
    assert_eq!(spec.image(), "mysql:8.0");
    assert_eq!(spec.ports(), [MYSQL_PORT]);
  }
}
