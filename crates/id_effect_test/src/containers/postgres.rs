//! [`Postgres`]: a PostgreSQL server ([`PostgresContainer`]), and with feature `sql` a ready
//! `SqlClient` over it (`Postgres::sql_layer`).

use std::time::Duration;

use super::{Container, ContainerImage, ContainerSpec, DEFAULT_READY_TIMEOUT, Wait};

/// PostgreSQL's container port.
pub const POSTGRES_PORT: u16 = 5432;

/// A PostgreSQL server (official `postgres` image, default tag `17-alpine`; user, password and
/// database `postgres`). Its data directory is a tmpfs and `fsync` is off: fast, and nothing
/// is left behind. Ready once `pg_isready` answers over TCP (after the image's init scripts).
#[derive(Clone, Debug)]
pub struct Postgres {
  tag: String,
  user: String,
  password: String,
  database: String,
  ready_timeout: Duration,
}

impl Default for Postgres {
  fn default() -> Self {
    Self::new()
  }
}

impl Postgres {
  /// `postgres:17-alpine`, user/password/database `postgres`.
  pub fn new() -> Self {
    Self {
      tag: "17-alpine".into(),
      user: "postgres".into(),
      password: "postgres".into(),
      database: "postgres".into(),
      ready_timeout: DEFAULT_READY_TIMEOUT,
    }
  }

  /// The image tag.
  pub fn tag(mut self, tag: impl Into<String>) -> Self {
    self.tag = tag.into();
    self
  }

  /// The superuser created at start (`POSTGRES_USER`).
  pub fn user(mut self, user: impl Into<String>) -> Self {
    self.user = user.into();
    self
  }

  /// Its password (`POSTGRES_PASSWORD`).
  pub fn password(mut self, password: impl Into<String>) -> Self {
    self.password = password.into();
    self
  }

  /// The database created at start (`POSTGRES_DB`).
  pub fn database(mut self, database: impl Into<String>) -> Self {
    self.database = database.into();
    self
  }

  /// Bounds the readiness wait ([`ContainerSpec::ready_timeout`]).
  pub fn ready_timeout(mut self, timeout: Duration) -> Self {
    self.ready_timeout = timeout;
    self
  }
}

impl ContainerImage for Postgres {
  type Running = PostgresContainer;

  fn spec(&self) -> ContainerSpec {
    ContainerSpec::new("postgres", &self.tag)
      .port(POSTGRES_PORT)
      .env("POSTGRES_USER", &self.user)
      .env("POSTGRES_PASSWORD", &self.password)
      .env("POSTGRES_DB", &self.database)
      .cmd(["postgres", "-c", "fsync=off"])
      .tmpfs("/var/lib/postgresql/data")
      .health_cmd([
        "pg_isready",
        "-h",
        "127.0.0.1",
        "-U",
        self.user.as_str(),
        "-d",
        self.database.as_str(),
      ])
      .wait(Wait::healthy())
      .ready_timeout(self.ready_timeout)
  }

  fn running(&self, container: Container) -> PostgresContainer {
    PostgresContainer {
      port: container.mapped_port(POSTGRES_PORT),
      container,
      user: self.user.clone(),
      password: self.password.clone(),
      database: self.database.clone(),
    }
  }

  fn container(running: &PostgresContainer) -> &Container {
    &running.container
  }
}

/// A started, ready PostgreSQL server: the capability [`Postgres::layer`](ContainerImage::layer)
/// provides.
#[derive(Clone, Debug)]
pub struct PostgresContainer {
  container: Container,
  port: u16,
  user: String,
  password: String,
  database: String,
}

impl PostgresContainer {
  /// `postgres://user:password@host:port/database`.
  pub fn url(&self) -> String {
    self.url_for(&self.database)
  }

  /// The URL of another database on the same server (one created by a test).
  pub fn url_for(&self, database: &str) -> String {
    format!(
      "postgres://{}:{}@{}:{}/{database}",
      self.user,
      self.password,
      self.container.host(),
      self.port
    )
  }

  /// The host.
  pub fn host(&self) -> &str {
    self.container.host()
  }

  /// The host port of [`POSTGRES_PORT`].
  pub fn port(&self) -> u16 {
    self.port
  }

  /// The superuser.
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

#[cfg(feature = "sql")]
mod sql {
  use id_effect::{HCons, HNil, Layer, need};
  use id_effect_sql::SqlError;
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, PgSqlEnv};

  use super::{Postgres, PostgresContainer};
  use crate::containers::{ContainerError, ContainerImage, Shared};

  /// What [`Postgres::sql_layer`] provides: the container, then the client and its pool.
  pub type PostgresSqlEnv = HCons<PostgresContainer, PgSqlEnv>;

  impl PostgresContainer {
    /// `PgSqlClient::layer` over [`url`](Self::url): a pool opened (and checked with
    /// `SELECT 1`) when the layer is built, closed when its scope closes. Needs a tokio runtime
    /// with its IO driver (sqlx), e.g. `id_effect_tokio::TokioRuntime`.
    pub fn sql_layer<E>(&self) -> Layer<PgSqlEnv, E>
    where
      E: From<SqlError> + Send + 'static,
    {
      PgSqlClient::layer(PgPoolConfig::from_url(self.url()))
    }
  }

  impl Postgres {
    /// A started container and a ready `SqlClient` over it, as one scoped layer: the pool
    /// closes, then the container is removed, when its scope closes.
    pub fn sql_layer<E>(&self) -> Layer<PostgresSqlEnv, E>
    where
      E: From<ContainerError> + From<SqlError> + Send + 'static,
    {
      client_over_container().provide_merge(self.layer())
    }
  }

  impl Shared<Postgres> {
    /// A ready `SqlClient` over the shared container: a pool per build, closed when the
    /// build's scope closes (the container stays).
    pub fn sql_layer<E>(&'static self) -> Layer<PostgresSqlEnv, E>
    where
      E: From<ContainerError> + From<SqlError> + Send + 'static,
    {
      client_over_container().provide_merge(self.layer())
    }
  }

  /// The client layer over the container the environment provides.
  fn client_over_container<E>() -> Layer<PgSqlEnv, E, HCons<PostgresContainer, HNil>>
  where
    E: From<SqlError> + Send + 'static,
  {
    Layer::scoped(|scope| {
      need::<PostgresContainer>().flat_map(move |pg| {
        pg.sql_layer::<E>()
          .build_scoped(&scope)
          .narrow::<HCons<PostgresContainer, HNil>, HNil>()
      })
    })
  }
}

#[cfg(feature = "sql")]
pub use sql::PostgresSqlEnv;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_builder_shapes_the_spec() {
    assert_eq!(Postgres::default().spec().image(), "postgres:17-alpine");
    let spec = Postgres::new()
      .tag("16-alpine")
      .ready_timeout(Duration::from_secs(9))
      .spec();
    assert_eq!(spec.image(), "postgres:16-alpine");
    assert_eq!(spec.ports(), [POSTGRES_PORT]);
  }
}
