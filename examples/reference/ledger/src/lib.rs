//! # Ledger — the id_effect reference app
//!
//! A multi-tenant double-entry ledger service, built only through `id_effect_kit`. It is the
//! application the 0.9 platform crates are tested against in CI (`examples/reference/ledger`,
//! ADR 0012): what it does here is what a service built on id_effect is expected to do.
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
//! | [`error`] | every failure as RFC 9457 Problem+JSON |
//!
//! `main.rs` loads the profile, opens the databases (applying `migrations/`), Redis and the
//! OpenID Connect provider, and serves [`app::router`] until SIGINT / SIGTERM.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod accounts;
pub mod app;
pub mod auth;
pub mod config;
pub mod db;
pub mod entries;
pub mod error;
pub mod policy;
pub mod reports;

pub use app::{App, AppEnv, Services, router, start};
pub use config::LedgerConfig;
pub use db::Database;
pub use error::{ApiError, StartupError};
