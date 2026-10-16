//! One front door to id_effect: a single dependency, a feature flag per battery and a
//! [`prelude`].
//!
//! ```toml
//! [dependencies]
//! id_effect_kit = { version = "0.8", features = ["web", "otel"] }
//! # The derive and `effect!` macros expand to `::id_effect::` paths; keep the core crate too.
//! id_effect = "0.8"
//! ```
//!
//! ## Features
//!
//! | Feature | Crate | Re-exported as |
//! |---|---|---|
//! | always on | `id_effect`, `id_effect_cli`, `id_effect_config` | [`id_effect`], [`cli`], [`config`] |
//! | `web` | `id_effect_axum` | `axum` |
//! | `sql-pg` | `id_effect_sql` with its `pg` driver | `sql` |
//! | `otel` | `id_effect_opentelemetry` (OTLP, config loading) | `otel` |
//! | `cluster` | `id_effect_node` (with `web`: also `axum::cluster`) | `node` |
//! | `full` | all of the above | |
//!
//! Each battery stays reachable whole under its short name (`id_effect_kit::axum::server`, …);
//! the [`prelude`] holds the items most programs touch.
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

/// SQL client capability with the PostgreSQL driver ([`id_effect_sql`]); feature `sql-pg`.
#[cfg(feature = "sql-pg")]
pub use id_effect_sql as sql;

/// Compiles and runs the README's quick start as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
