//! One front door to id_effect: a single dependency, a feature flag per battery and a
//! [`prelude`].
//!
//! ```toml
//! [dependencies]
//! id_effect_kit = { version = "0.9", features = ["web", "otel"] }
//! # The derive and `effect!` macros expand to `::id_effect::` paths; keep the core crate too.
//! id_effect = "0.9"
//! ```
//!
//! ## Features
//!
//! | Feature | Crate | Re-exported as |
//! |---|---|---|
//! | always on | `id_effect`, `id_effect_cli`, `id_effect_config` | [`id_effect`], [`cli`], [`config`] |
//! | `web` | `id_effect_axum` (typed routes, Problem+JSON, middleware) | `axum` |
//! | `openapi` | `web` + id_effect_axum's `openapi` (`/openapi.json`, `/docs`) | `axum::openapi` |
//! | `sql` | `id_effect_sql` without a driver | `sql` |
//! | `sql-pg`, `sql-sqlite`, `sql-mysql` | `sql` + its `pg` / `sqlite` / `mysql` driver | `sql::pg`, `sql::sqlite`, `sql::mysql` |
//! | `migrate` | `sql` + versioned migrations | `sql::migrate` |
//! | `repository` | `sql` + `#[derive(Entity)]` and `Repository<T>` | `sql::repository` |
//! | `audit` | `sql` + the hash-chained audit log (PostgreSQL) | `sql::audit` |
//! | `otel` | `id_effect_opentelemetry` (OTLP, config loading) | `otel` |
//! | `cluster` | `id_effect_node` (with `web`: also `axum::cluster`) | `node` |
//! | `auth` | `web` + `id_effect_auth` with its Axum middleware and JWTs (with `sql-pg`: `PgSessionStore`; with `redis`: the Redis session store) | `auth` |
//! | `auth-oidc`, `auth-totp` | `auth` + OpenID Connect (and JWKS) / TOTP | `auth::oidc`, `auth::totp` |
//! | `authz` | `id_effect_authz` (with `web`: the route guards) | `authz` |
//! | `redis` | `id_effect_redis` (with `web`: the rate-limit store) | `redis` |
//! | `config-vault` | id_effect_config's Vault provider | `config::vault` |
//! | `jobs` | `id_effect_jobs` (PostgreSQL queue, workers, retries; with `mail`: `deliver_later`; with `console`: the jobs panel) | `jobs` |
//! | `jobs-cron` | `jobs` + cron entries that enqueue jobs once per cluster | `jobs::CronLeader` |
//! | `messaging` | `id_effect_messaging` (`MessageBroker`, in-memory broker, outbox, inbox, relay) | `messaging` |
//! | `redis-streams` | `redis` + `messaging` + the broker on Redis Streams | `redis::RedisStreamsBroker` |
//! | `mail` | `id_effect_mail` (SMTP, SendGrid, templates; with `web`: the preview routes) | `mail` |
//! | `console` | `web` + `authz` + `id_effect_console` (the guarded operator console) | `console` |
//! | `storage` | `id_effect_storage` (local and in-memory object stores, signed URLs) | `storage` |
//! | `storage-s3` | `storage` + the S3 (and S3-compatible) store | `storage::S3Store` |
//! | `http-client` | `id_effect_platform` (the `HttpClient` capability, reqwest, retries; with `otel`: `TracedHttpClient`) | `platform` |
//! | `logger` | `id_effect_logger` (`EffectLogger`, backends, `RedactionPolicy`) | `logger` |
//! | `events` | `id_effect_events` (event store, `SqlEventJournal`, projections, snapshots) | `events` |
//! | `auth-accounts` | `auth` + `mail` + user accounts (`Accounts`, `AccountRoutes`, `PgAccountTokenStore`; `AccountMails`) | `auth`, `mail` |
//! | `test` | `id_effect_test` (containers, isolated databases, fixtures, HTTP client); for `[dev-dependencies]` | `test` |
//! | `full` | all of the above except `test` | |
//!
//! Each battery stays reachable whole under its short name (`id_effect_kit::axum::server`, …);
//! the [`prelude`] holds the items most programs touch.
//!
//! `test` is meant for tests only: enable it on a `[dev-dependencies]` entry of the kit, so
//! containers and fixtures stay out of the shipped binary:
//!
//! ```toml
//! [dependencies]
//! id_effect_kit = { version = "0.9", features = ["web", "sql-pg", "migrate"] }
//! [dev-dependencies]
//! id_effect_kit = { version = "0.9", features = ["test"] }
//! ```
//!
//! [`graph::LayerGraph`] registers an app's layers by name and renders their dependency edges
//! as text or DOT, flagging capabilities nobody provides (`id-effect doctor --graph`).
//!
//! ## Quick start
//!
//! ```rust
//! use id_effect_kit::prelude::*;
//!
//! fn greet(name: &'static str) -> Effect<String, String> {
//!   if name.is_empty() {
//!     fail("no name".to_string())
//!   } else {
//!     succeed(format!("hello, {name}"))
//!   }
//! }
//!
//! assert!(matches!(greet("kit").block_on(), Exit::Success(s) if s == "hello, kit"));
//! ```
//!
//! In a binary, hand the program to [`run_main`](prelude::run_main) from `main`; it maps the
//! [`Exit`](prelude::Exit) to the process exit code.
//!
//! ## Examples
//!
//! See `examples/` (e.g. `cargo run -p id_effect_kit --example 010_prelude`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod graph;
pub mod prelude;

pub use id_effect;
/// The CLI program edge ([`id_effect_cli`]): `run_main`, exit codes.
pub use id_effect_cli as cli;
/// Layered configuration ([`id_effect_config`]): providers, `Secret`.
pub use id_effect_config as config;

/// HTTP on Axum ([`id_effect_axum`]); feature `web`.
#[cfg(feature = "web")]
pub use id_effect_axum as axum;

/// Distribution and clustering ([`id_effect_node`]); feature `cluster`.
#[cfg(feature = "cluster")]
pub use id_effect_node as node;

/// OpenTelemetry export ([`id_effect_opentelemetry`]); feature `otel`.
#[cfg(feature = "otel")]
pub use id_effect_opentelemetry as otel;

/// SQL client capability ([`id_effect_sql`]); feature `sql`, turned on by `sql-pg`,
/// `sql-sqlite`, `sql-mysql`, `migrate` and `repository` (which add its drivers and modules).
#[cfg(feature = "sql")]
pub use id_effect_sql as sql;

/// Authentication ([`id_effect_auth`]) with its Axum middleware and JWTs; feature `auth`.
#[cfg(feature = "auth")]
pub use id_effect_auth as auth;

/// Authorization policies ([`id_effect_authz`]); feature `authz`.
#[cfg(feature = "authz")]
pub use id_effect_authz as authz;

/// Redis client, cache backend and stores ([`id_effect_redis`]); feature `redis`.
#[cfg(feature = "redis")]
pub use id_effect_redis as redis;

/// Background jobs ([`id_effect_jobs`]): queues, workers, retries; feature `jobs` (cron entries
/// with `jobs-cron`).
#[cfg(feature = "jobs")]
pub use id_effect_jobs as jobs;

/// Broker-agnostic messaging and the transactional outbox ([`id_effect_messaging`]); feature
/// `messaging`.
#[cfg(feature = "messaging")]
pub use id_effect_messaging as messaging;

/// Outgoing mail ([`id_effect_mail`]): SMTP, SendGrid, templates; feature `mail`.
#[cfg(feature = "mail")]
pub use id_effect_mail as mail;

/// The operator console ([`id_effect_console`]), behind an authorization guard; feature
/// `console`.
#[cfg(feature = "console")]
pub use id_effect_console as console;

/// Object storage ([`id_effect_storage`]); feature `storage`.
#[cfg(feature = "storage")]
pub use id_effect_storage as storage;

/// Outbound HTTP ([`id_effect_platform`]): the `HttpClient` capability and the reqwest client;
/// feature `http-client`.
#[cfg(feature = "http-client")]
pub use id_effect_platform as platform;

/// Structured logging ([`id_effect_logger`]) and its `RedactionPolicy`; feature `logger`.
#[cfg(feature = "logger")]
pub use id_effect_logger as logger;

/// Event sourcing ([`id_effect_events`]); feature `events`.
#[cfg(feature = "events")]
pub use id_effect_events as events;

/// Test support ([`id_effect_test`]): containers, isolated databases, fixtures, the in-process
/// HTTP client; feature `test`, meant for `[dev-dependencies]`.
#[cfg(feature = "test")]
pub use id_effect_test as test;

/// Compiles and runs the README's quick start as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
