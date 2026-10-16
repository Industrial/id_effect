//! Integration-test support for id_effect programs.
//!
//! - **[`containers`]** — effect-native containers: [`Postgres`] (with feature `sql`, a ready
//!   `SqlClient` via `Postgres::sql_layer`), [`MySql`], [`Redis`], [`Keycloak`], [`Vault`] and
//!   any image through [`ContainerSpec`], as scoped layers removed when their scope closes
//!   (also on failure and interrupt), or one per test binary with [`Shared`]; readiness waits
//!   bounded by typed timeouts ([`ContainerError::NotReady`]); [`require_docker`] for the SKIP /
//!   `ID_EFFECT_REQUIRE_DOCKER=1` rule.
//!
//! Per-test database isolation and fixtures land with the next 0.9 leaf (E6.5).
//!
//! ## Cargo features
//!
//! - **`sql`** — `Postgres::sql_layer` / `PostgresContainer::sql_layer`: an `id_effect_sql`
//!   `PgSqlClient` over the container (needs a tokio runtime, as sqlx does).
//!
//! Container tests run in CI's containers job with `ID_EFFECT_REQUIRE_DOCKER=1`.
//!
//! Tier `integration` (ADR 0010 Amendment 5); `platform` once the Ledger reference app
//! under `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod containers;

pub use containers::{
  Container, ContainerError, ContainerImage, ContainerSpec, Keycloak, KeycloakContainer, MySql,
  MySqlContainer, Postgres, PostgresContainer, REQUIRE_DOCKER_ENV, Redis, RedisContainer, Shared,
  Vault, VaultContainer, Wait, container_exists, docker_available, require_docker,
};

/// Compiles and runs the README's code block as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// Compiles the book chapter on containers as a doctest (its blocks are `no_run`: they need
/// Docker).
#[cfg(doctest)]
#[doc = include_str!("../../id_effect/book/src/part4/ch15-07-containers.md")]
struct BookContainersDoctests;
