//! Integration-test support for id_effect programs.
//!
//! - **[`containers`]** — effect-native containers: [`Postgres`] (with feature `sql`, a ready
//!   `SqlClient` via `Postgres::sql_layer`), [`MySql`], [`Redis`], [`Keycloak`], [`Vault`] and
//!   any image through [`ContainerSpec`], as scoped layers removed when their scope closes
//!   (also on failure and interrupt), or one per test binary with [`Shared`]; readiness waits
//!   bounded by typed timeouts ([`ContainerError::NotReady`]); [`require_docker`] for the SKIP /
//!   `ID_EFFECT_REQUIRE_DOCKER=1` rule.
//!
//! - **`db`** (feature `sql`) — per-test database isolation: `isolated_db()` runs the test in one
//!   transaction that is always rolled back (any `SqlClient`: PostgreSQL, SQLite, …);
//!   `TemplateDb` migrates a template database once per test binary and clones it into a fresh
//!   database per test, for code that commits.
//! - **`fixtures`** (feature `fixtures`) — `Fixture` / `Factory`: rows with deterministic,
//!   Schema-arbitrary defaults (a seeded generator), overrides, inserted and read back with
//!   `FromRow`.
//! - **`http`** (feature `http`) — `TestClient`: the in-process HTTP client over a tower
//!   `Service` (an axum `Router`): no socket; status, headers, JSON and problem bodies.
//!
//! ## Cargo features
//!
//! - **`sql`** — `Postgres::sql_layer` / `PostgresContainer::sql_layer`: an `id_effect_sql`
//!   `PgSqlClient` over the container (needs a tokio runtime, as sqlx does), and `db`.
//! - **`migrate`** — `db::migrate`: an app's migration directory as a `TemplateDb` setup.
//! - **`fixtures`** — `fixtures` (with `sql`; proptest).
//! - **`http`** — `http` (tower and `http` types; no axum dependency).
//!
//! Container tests run in CI's containers job with `ID_EFFECT_REQUIRE_DOCKER=1`.
//!
//! Tier `integration` (ADR 0010 Amendment 5); `platform` once the Ledger reference app
//! under `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod containers;
#[cfg(feature = "sql")]
pub mod db;
#[cfg(feature = "fixtures")]
pub mod fixtures;
#[cfg(feature = "http")]
pub mod http;

pub use containers::{
  Container, ContainerError, ContainerImage, ContainerSpec, Keycloak, KeycloakContainer, MySql,
  MySqlContainer, Postgres, PostgresContainer, REQUIRE_DOCKER_ENV, Redis, RedisContainer, Shared,
  Vault, VaultContainer, Wait, container_exists, docker_available, require_docker,
};
#[cfg(feature = "sql")]
pub use db::{IsolatedDatabase, TemplateDb, TransactionSqlClient, isolated_db};
#[cfg(feature = "fixtures")]
pub use fixtures::{Factory, Fixture};
#[cfg(feature = "http")]
pub use http::{HttpTestError, Problem, TestClient, TestRequest, TestResponse};

/// Compiles and runs the README's code block as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// Compiles the book chapter on containers as a doctest (its blocks are `no_run`: they need
/// Docker).
#[cfg(doctest)]
#[doc = include_str!("../../id_effect/book/src/part4/ch15-07-containers.md")]
struct BookContainersDoctests;

/// Runs the book chapter on integration testing (isolated databases, fixtures, the HTTP test
/// client) as a doctest.
#[cfg(all(doctest, feature = "fixtures", feature = "http", feature = "migrate"))]
#[doc = include_str!("../../id_effect/book/src/part4/ch15-08-integration-testing.md")]
struct BookIntegrationTestingDoctests;
