//! The driver conformance suite (`mod.rs`), instantiated once per backend feature: one generic
//! module, the same tests on every database behind `SqlClient` (data.backends #6).
//!
//! - `pg`: PostgreSQL at `DATABASE_URL` (skipped without it unless `ID_EFFECT_REQUIRE_PG=1`).
//! - `sqlite`: SQLite in memory, in a file, and in memory behind a `RoutingSqlClient`.
//!
//! A new backend adds `mod <backend>;` here and calls `conformance_suite!` in it.

#[allow(dead_code, unused_macros)]
#[path = "mod.rs"]
mod conformance;

#[cfg(feature = "pg")]
mod pg;
#[cfg(feature = "sqlite")]
mod sqlite;
