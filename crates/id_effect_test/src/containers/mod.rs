//! Effect-native containers (testcontainers): PostgreSQL, MySQL, Redis, Keycloak, Vault, an
//! S3-compatible store, a Mailpit SMTP server and any image through [`ContainerSpec`], started as
//! effect resources.
//!
//! # Per-test containers
//!
//! Every image is a [`ContainerImage`]. [`ContainerImage::layer`] is a scoped [`Layer`]: the
//! container starts (and passes its readiness wait) when the layer is built and is removed when
//! the build's scope closes — after the effect `provide_layer` ran, whether it succeeded,
//! failed, died or was interrupted. [`ContainerImage::acquire`] does the same into a [`Scope`]
//! you hold.
//!
//! ```no_run
//! use id_effect::{Effect, caps, need};
//! use id_effect_test::{ContainerError, ContainerImage, Redis, RedisContainer};
//!
//! let url: Effect<String, ContainerError, caps!(RedisContainer)> =
//!   need::<RedisContainer>().map(|redis| redis.url());
//! let url = url.provide_layer(Redis::new().layer());
//! let id_effect::Exit::Success(url) = url.block_on() else { panic!("no Redis") };
//! assert!(url.starts_with("redis://"));
//! // The container is gone again here.
//! ```
//!
//! # Shared containers
//!
//! [`Shared`] keeps one container per test binary in a `static`, started by the first test that
//! asks for it. Statics are never dropped, so the first shared start also starts the reaper
//! (testcontainers' Ryuk): this process holds a connection to it, and when the process exits it
//! removes every container this process started. `TESTCONTAINERS_RYUK_DISABLED=true` turns the
//! reaper off.
//!
//! # Readiness
//!
//! A start waits for its [`Wait`] strategies (a log line, the health check, a delay) under the
//! spec's readiness timeout ([`DEFAULT_READY_TIMEOUT`], 180 s for Keycloak). A container that is
//! not ready in time is removed and the start fails with [`ContainerError::NotReady`]; one that
//! exits or turns unhealthy first, with [`ContainerError::Unready`].
//!
//! A start that loses the race for a random host port (`address already in use`, common under
//! rootless Docker, or `port is already allocated`) is removed and retried as a new container
//! with fresh host ports, a few times with a short backoff, each retry logged to stderr; any
//! other start error fails at once with [`ContainerError::Start`].
//!
//! # Docker
//!
//! Docker is found as testcontainers finds it (`DOCKER_HOST`, `~/.testcontainers.properties`,
//! the default socket). Guard container tests with [`require_docker`]: without Docker the test
//! prints `SKIP` and passes, unless `ID_EFFECT_REQUIRE_DOCKER=1` (CI's containers job), where it
//! fails. Docker calls run on a runtime owned by this crate, so the effects run on any executor
//! (`Effect::block_on`, `id_effect_tokio::TokioRuntime`); only the `sql` layers need tokio for
//! sqlx.
//!
//! [`Layer`]: id_effect::Layer
//! [`Scope`]: id_effect::Scope

mod container;
mod docker;
mod error;
mod image;
mod keycloak;
mod mailpit;
mod mysql;
mod postgres;
mod reaper;
mod redis;
mod s3;
mod shared;
mod spec;
mod vault;
mod wait;

pub use container::Container;
pub use docker::{REQUIRE_DOCKER_ENV, container_exists, docker_available, require_docker};
pub use error::ContainerError;
pub use image::ContainerImage;
pub use keycloak::{KEYCLOAK_PORT, KEYCLOAK_READY_TIMEOUT, Keycloak, KeycloakContainer};
pub use mailpit::{
  MAILPIT_API_PORT, MAILPIT_IMAGE, MAILPIT_SMTP_PORT, MAILPIT_TAG, Mailpit, MailpitContainer,
};
pub use mysql::{MYSQL_PORT, MySql, MySqlContainer};
#[cfg(feature = "sql")]
pub use postgres::PostgresSqlEnv;
pub use postgres::{POSTGRES_PORT, Postgres, PostgresContainer};
pub use redis::{REDIS_PORT, Redis, RedisContainer};
pub use s3::{S3, S3_IMAGE, S3_PORT, S3_TAG, S3Container};
pub use shared::Shared;
pub use spec::{ContainerSpec, DEFAULT_READY_TIMEOUT};
pub use vault::{VAULT_PORT, Vault, VaultContainer};
pub use wait::Wait;
