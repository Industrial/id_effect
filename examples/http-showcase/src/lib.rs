//! The id_effect 0.7 showcase: an axum HTTP service whose whole lifetime is one effect on the
//! fiber runtime, laid out in domain-driven layers. Every struct, enum, trait, type alias and
//! function has its own file.
//!
//! | Layer | Holds | Depends on |
//! |---|---|---|
//! | [`domain`] | the `todo` context: the [`Todo`] aggregate, its value objects and errors, the [`TodoRepository`] port | nothing |
//! | [`application`] | one use case per endpoint, the [`RequestPolicy`] (circuit breaker, retry, timeout), the [`AppEnv`] | domain |
//! | [`infrastructure`] | the in-memory and PostgreSQL adapters, [`AppConfig`], telemetry | domain |
//! | [`interface`] | the HTTP routes: `Schema` request decoding, error-to-status mapping | application, domain |
//! | [`bootstrap`] | the layer graph that starts it all | everything |
//!
//! Every use case declares exactly the errors it can fail with (`err!(A | B | ..)`), and only
//! the HTTP layer turns them into statuses.
//!
//! ```rust,no_run
//! use id_effect::Exit;
//! use id_effect_axum::EffectExecutor;
//!
//! # async fn run() {
//! let program = http_showcase::run_until_shutdown().provide_layer(http_showcase::live_layer());
//! match EffectExecutor::global().run_effect(program).await {
//!   Exit::Success(reason) => println!("stopped: {reason:?}"),
//!   Exit::Failure(cause) => eprintln!("failed: {cause:?}"),
//! }
//! # }
//! ```

#![deny(missing_docs)]
// Error unions (`err!(A | B | ..)`) and capability lists (`caps!(..)`) are the contracts of
// this crate's signatures; naming each one would hide exactly what they show.
#![allow(clippy::type_complexity)]

pub mod application;
pub mod bootstrap;
pub mod domain;
pub mod infrastructure;
pub mod interface;

pub use application::{AppEnv, RequestPolicy, new_app_env};
pub use bootstrap::{StartupError, live_layer, repository_layer, run_until, run_until_shutdown};
pub use domain::todo::{Todo, TodoRepository, TodoRepositoryService};
pub use infrastructure::config::{AppConfig, DatabaseConfig};
pub use infrastructure::memory::InMemoryTodoRepository;
pub use infrastructure::postgres::{PgTodoRepository, database_layer};
pub use interface::http::router;
