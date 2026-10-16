//! The composition root: the layer graph that turns the configuration into a running service.
//!
//! ```text
//! env  ──►  AppConfig (live_layer)
//!             ├──►  HostConfig  ──►  server (scoped: bind, log, graceful drain)
//!             └──►  repository_layer: in memory, or PgSqlClient pool ──► migrated PgTodoRepository
//!                     └──►  router(AppEnv = store + RequestPolicy), instrumented
//! ```

mod live_layer;
mod log_startup;
mod logging_layer;
mod repository_layer;
mod run_until;
mod run_until_shutdown;
mod serve_router;
mod startup_error;

pub use live_layer::live_layer;
pub use log_startup::log_startup;
pub use logging_layer::logging_layer;
pub use repository_layer::repository_layer;
pub use run_until::run_until;
pub use run_until_shutdown::run_until_shutdown;
pub use serve_router::serve_router;
pub use startup_error::StartupError;
