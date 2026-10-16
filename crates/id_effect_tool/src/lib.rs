//! The `id-effect` command (ADR 0010 Amendment 4): `id-effect new` generates a project on
//! [`id_effect_kit`](https://docs.rs/id_effect_kit) from one of four templates embedded in the
//! binary.
//!
//! ```text
//! cargo install id_effect_tool
//! id-effect new orders-api --template service [--db pg] [--otel]
//! id-effect new nightly --template worker
//! id-effect new deployer --template cli
//! id-effect new mesh-node --template cluster
//! ```
//!
//! | Template | Generates |
//! |---|---|
//! | `service` | Axum app: `SecurityLayer::recommended`, `/health` + `/ready` (`HealthRegistry`), `/metrics` + `http_metrics_layer`; `--db pg` adds an SQL client and a migration, `--otel` OTLP export |
//! | `worker` | `CronRegistry` jobs with run history under `run_main` |
//! | `cli` | clap + `run_main` + shell completions |
//! | `cluster` | an `id_effect_node` mesh node hosting a process behavior |
//!
//! Every project gets a `Cargo.toml` on `id_effect_kit` (plus `id_effect`, which the macros
//! expand to), `src/`, `tests/`, a `Dockerfile`, `compose.yaml`, `.github/workflows/ci.yml`,
//! `.env.example` and a README. `--local-workspace PATH` builds it against an id_effect checkout.
//!
//! The library is what the binary runs, usable on its own:
//!
//! ```rust
//! use id_effect_tool::{Options, Template, render};
//!
//! let files = render(&Options::new("orders-api", Template::Service))?;
//! let cargo = files.iter().find(|f| f.path == "Cargo.toml").unwrap();
//! assert!(cargo.contents.contains("name = \"orders-api\""));
//! # Ok::<(), id_effect_tool::NewError>(())
//! ```
//!
//! [`new_project`] writes the files (refusing a non-empty destination); [`cli::run`] is the
//! whole command line.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod cli;
pub mod doctor;
pub mod error;
pub mod project;
pub mod render;
pub mod template;

pub use error::NewError;
pub use project::{Report, new_project};
pub use render::{Db, ID_EFFECT_VERSION, Options, RenderedFile, render};
pub use template::Template;

/// Compiles and runs the README's example as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
