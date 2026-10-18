//! # Ledger — the id_effect reference app
//!
//! A multi-tenant double-entry ledger service, built only through `id_effect_kit`. It is the
//! application the 0.9 and 0.10 platform crates are tested against in CI
//! (`examples/reference/ledger`, ADR 0012): what it does here is what a service built on
//! id_effect is expected to do.
//!
//! | Module | What |
//! |---|---|
//! | [`config`] | profiles `dev` / `test` / `prod`; `prod` reads its secrets from Vault |
//! | [`db`] | the primary, read replicas behind a `RoutingSqlClient`, migrations at start |
//! | [`app`] | the shared state, the caller of a request, the router and its middleware |
//! | [`policy`] | roles, permissions and the named policies (`can_post`, ownership) |
//! | [`auth`] | password + TOTP login into PostgreSQL sessions, API tokens (JWT bearer) |
//! | [`accounts`] | the chart of accounts: tenant-scoped, soft-deleted, optimistically locked |
//! | [`entries`] | journal entries posted in `SERIALIZABLE` transactions, batches with savepoints |
//! | [`reports`] | balances (Redis-cached, `ETag`ged), trial balance, CSV export, audit check |
//! | [`signup`] | registration, email verification and password reset (id_effect_auth accounts) |
//! | [`limits`] | per-client and per-organisation login limits, reloaded from the profile file |
//! | [`notifications`] | postings announced through the outbox and Redis Streams; balance notifications with inbox dedup |
//! | [`statements`] | the monthly statement CSV and how it is delivered |
//! | [`jobs`] | the statement cron (one leader), the statement jobs and the mail queue |
//! | [`mail`] | SMTP / SendGrid over traced HTTP, minijinja templates, `/_mail/previews` (dev) |
//! | [`console`] | the operator console at `/_console` |
//! | [`observability`] | OpenTelemetry from the environment, the redaction policy |
//! | [`background`] | the worker, the cron, the relay and the consumer of one instance |
//! | [`error`] | every failure as RFC 9457 Problem+JSON |
//!
//! `main.rs` loads the profile, opens the databases (applying `migrations/`), Redis, the mail
//! transport, the statements bucket and the OpenID Connect provider, starts the
//! [`background`] tasks and serves [`app::router`] until SIGINT / SIGTERM.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod accounts;
pub mod app;
pub mod auth;
pub mod background;
pub mod config;
pub mod console;
pub mod db;
pub mod entries;
pub mod error;
pub mod jobs;
pub mod limits;
pub mod mail;
pub mod notifications;
pub mod observability;
pub mod policy;
pub mod reports;
pub mod signup;
pub mod statements;

pub use app::{App, AppEnv, Services, router, start};
pub use config::LedgerConfig;
pub use db::Database;
pub use error::{ApiError, StartupError};
