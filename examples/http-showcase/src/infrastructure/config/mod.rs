//! Configuration: every setting of the showcase as one `Config<T>` descriptor
//! ([`AppConfig::config`]), provided to the program as a capability by [`AppConfig::layer`]
//! (Effect.ts `Config` read through a `Layer`).
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `HOST` | `0.0.0.0` | bind address ([`HostConfig`]) |
//! | `PORT` | `8080` | listen port ([`HostConfig`]) |
//! | `SHUTDOWN_TIMEOUT_SECS` | `30` | graceful drain on shutdown ([`HostConfig`]) |
//! | `REQUEST_TIMEOUT` | `2s` | per-request handler timeout (a duration: `500ms`, `2s`, ...) |
//! | `DATABASE_URL` | unset | PostgreSQL URL; unset or blank runs without a database |
//! | `DATABASE_MAX_CONNECTIONS` | `16` | pool size when `DATABASE_URL` is set |
//!
//! Every key is read together ([`Config::all`]), so a misconfigured process reports all of its
//! bad keys at once.
//!
//! [`HostConfig`]: id_effect_axum::HostConfig
//! [`Config::all`]: id_effect_config::Config::all

mod app_config;
mod database_config;

pub use app_config::AppConfig;
pub use database_config::DatabaseConfig;
