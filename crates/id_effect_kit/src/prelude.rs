//! `use id_effect_kit::prelude::*;` — the items most programs touch.
//!
//! | Source | Items | Feature |
//! |---|---|---|
//! | `id_effect` | [`Effect`], [`Exit`], [`Cause`], [`Or`], [`err!`], [`succeed`], [`fail`], [`from_async`], [`sleep`], [`Layer`], [`Schedule`], [`Schema`](struct@Schema) (type and derive), [`EffectData`], [`effect!`], [`caps`], [`hlist!`], [`need`] | always |
//! | `id_effect_cli` | [`run_main`], [`RunMainConfig`] | always |
//! | `id_effect_config` | [`Secret`], [`Profiles`] | always |
//! | `id_effect_axum` | `EffectExecutor`, `execute`, `routing`, `serve_until_shutdown`, `SecurityLayer`, `health_router`, `readiness_router`, `observability_routes`, `metrics_router`; typed routes `Route`, `Router`, `Input`, `Problem`, `ValidPath`, `ValidQuery`, `ValidHeaders`, `ValidJson`, `ValidForm`; middleware `RequestIdLayer`, `TimeoutLayer`, `BodyLimitLayer`, `CompressionLayer` | `web` |
//! | `id_effect_axum::openapi` | `OpenApi`, `openapi_router` | `openapi` |
//! | `id_effect_sql` | `SqlClientService`, `SqlError`, `SqlRow`, `FromRow` (trait and derive), `params!`, `with_transaction` | `sql` (any `sql-*`, `migrate`, `repository`) |
//! | `id_effect_sql::migrate` | `Migrator`, `MigrateError` | `migrate` |
//! | `id_effect_sql` | `Entity` (trait and derive), `Repository` | `repository` |
//! | `id_effect_opentelemetry` | `OtelConfig`, `install_from_config` | `otel` |
//! | `id_effect_auth` | `Authenticate`, `AuthPrincipal`, `AuthError`, `Sessions`, `SessionLayer`, `PasswordHasher`, `JwtIssuer`, `JwtVerifier`, `JwtKey` | `auth` |
//! | `id_effect_authz` | `Policies`, `Policy`, `Principal`, `AuthzError`, `CurrentPrincipal`, `authorize`, `require_role`; with `web`, `AuthorizeExt` | `authz` |
//! | `id_effect_redis` | `RedisClient`, `RedisConfig` | `redis` |
//! | `id_effect_jobs` | `Job`, `JobContext`, `JobError`, `JobFailure`, `JobOptions`, `JobQueue`, `JobQueueExt`, `JobRegistry`, `Worker`, `WorkerConfig` | `jobs` |
//! | `id_effect_jobs` | `CronLeader` | `jobs-cron` |
//! | `id_effect_messaging` | `MessageBroker`, `MessageBrokerExt`, `BrokerError`, `Delivery`, `Outbox`, `Relay` | `messaging` |
//! | `id_effect_redis` | `RedisStreamsBroker` | `redis-streams` |
//! | `id_effect_mail` | `Mailer`, `MailError`, `Mailbox`, `Templates`; with `jobs`, `deliver_later` | `mail` |
//! | `id_effect_console` | `Console`, `ConsoleConfig` | `console` |
//! | `id_effect_storage` | `ObjectStore`, `ObjectStorage`, `StorageError`, `LocalStore`, `MemoryStore` | `storage` |
//! | `id_effect_events` | `EventStore`, `EventStoreError`, `Projection`, `SqlEventJournal` | `events` |
//!
//! `Router` is the typed router of `id_effect_axum::route` (it records every route for OpenAPI
//! and `/_routes`); an explicit `use axum::Router;` still wins over the glob. `Problem` is
//! `id_effect_axum`'s RFC 9457 body, the one the extractors, middleware and guards answer with.
//! Names that would clash stay out: `id_effect_authz::Or` (the policy combinator; [`Or`] here
//! is the error channel's), `id_effect_test::Problem` (the test client's parsed copy), the two
//! `Message` types (`id_effect_messaging`'s and `id_effect_mail`'s) and the free functions of
//! `id_effect_storage` (`get`, `put`, `list`, …; call them as `storage::get`).
//!
//! The `FromRow` and `Entity` derives name `::id_effect_sql`; through the kit alone, point them
//! at the re-export: `#[from_row(crate = id_effect_kit::sql)]`, `#[entity(crate =
//! id_effect_kit::sql, …)]`.
//!
//! `cluster` adds nothing here; reach for `id_effect_kit::node`. `test` adds nothing either:
//! its helpers stay under `id_effect_kit::test`. Anything else stays one path away under the
//! crate re-exports (`id_effect_kit::id_effect::…`, `id_effect_kit::axum::…`).

pub use id_effect::{
  Cause, Effect, EffectData, Exit, Layer, Or, Schedule, Schema, caps, effect, err, fail,
  from_async, hlist, need, sleep, succeed,
};
pub use id_effect_cli::{RunMainConfig, run_main};
pub use id_effect_config::{Profiles, Secret};

#[cfg(feature = "web")]
pub use id_effect_axum::metrics::metrics_router;
#[cfg(feature = "web")]
pub use id_effect_axum::middleware::{
  BodyLimitLayer, CompressionLayer, RequestIdLayer, TimeoutLayer,
};
#[cfg(feature = "web")]
pub use id_effect_axum::route::{
  Input, Problem, Route, Router, ValidForm, ValidHeaders, ValidJson, ValidPath, ValidQuery,
};
#[cfg(feature = "web")]
pub use id_effect_axum::{
  EffectExecutor, SecurityLayer, execute, health_router, observability_routes, readiness_router,
  routing, serve_until_shutdown,
};

#[cfg(feature = "openapi")]
pub use id_effect_axum::openapi::{OpenApi, openapi_router};

#[cfg(feature = "sql")]
pub use id_effect_sql::{FromRow, SqlClientService, SqlError, SqlRow, params, with_transaction};

#[cfg(feature = "migrate")]
pub use id_effect_sql::migrate::{MigrateError, Migrator};

#[cfg(feature = "repository")]
pub use id_effect_sql::{Entity, Repository};

#[cfg(feature = "otel")]
pub use id_effect_opentelemetry::{OtelConfig, install_from_config};

#[cfg(feature = "auth")]
pub use id_effect_auth::{
  AuthError, AuthPrincipal, Authenticate, JwtIssuer, JwtKey, JwtVerifier, PasswordHasher,
  SessionLayer, Sessions,
};

#[cfg(all(feature = "authz", feature = "web"))]
pub use id_effect_authz::AuthorizeExt;
#[cfg(feature = "authz")]
pub use id_effect_authz::{
  AuthzError, CurrentPrincipal, Policies, Policy, Principal, authorize, require_role,
};

#[cfg(feature = "redis")]
pub use id_effect_redis::{RedisClient, RedisConfig};

#[cfg(feature = "jobs")]
pub use id_effect_jobs::{
  Job, JobContext, JobError, JobFailure, JobOptions, JobQueue, JobQueueExt, JobRegistry, Worker,
  WorkerConfig,
};

#[cfg(feature = "jobs-cron")]
pub use id_effect_jobs::CronLeader;

#[cfg(feature = "messaging")]
pub use id_effect_messaging::{
  BrokerError, Delivery, MessageBroker, MessageBrokerExt, Outbox, Relay,
};

#[cfg(feature = "redis-streams")]
pub use id_effect_redis::RedisStreamsBroker;

#[cfg(all(feature = "mail", feature = "jobs"))]
pub use id_effect_mail::deliver_later;
#[cfg(feature = "mail")]
pub use id_effect_mail::{MailError, Mailbox, Mailer, Templates};

#[cfg(feature = "console")]
pub use id_effect_console::{Console, ConsoleConfig};

#[cfg(feature = "storage")]
pub use id_effect_storage::{LocalStore, MemoryStore, ObjectStorage, ObjectStore, StorageError};

#[cfg(feature = "events")]
pub use id_effect_events::{EventStore, EventStoreError, Projection, SqlEventJournal};
