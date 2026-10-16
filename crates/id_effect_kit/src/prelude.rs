//! `use id_effect_kit::prelude::*;` — the items most programs touch.
//!
//! | Source | Items | Feature |
//! |---|---|---|
//! | `id_effect` | [`Effect`], [`Exit`], [`Cause`], [`Or`], [`err!`], [`succeed`], [`fail`], [`from_async`], [`sleep`], [`Layer`], [`Schedule`], [`Schema`](struct@Schema) (type and derive), [`EffectData`], [`effect!`], [`caps`], [`hlist!`], [`need`] | always |
//! | `id_effect_cli` | [`run_main`], [`RunMainConfig`] | always |
//! | `id_effect_config` | [`Secret`] | always |
//! | `id_effect_axum` | `EffectExecutor`, `execute`, `routing`, `serve_until_shutdown`, `SecurityLayer`, `health_router`, `readiness_router`, `observability_routes`, `metrics_router` | `web` |
//! | `id_effect_sql` | `SqlClientService`, `SqlError`, `with_transaction` | `sql-pg` |
//! | `id_effect_opentelemetry` | `OtelConfig`, `install_from_config` | `otel` |
//!
//! `cluster` adds nothing here; reach for `id_effect_kit::node`. Anything else stays one path
//! away under the crate re-exports (`id_effect_kit::id_effect::…`, `id_effect_kit::axum::…`).

pub use id_effect::{
  Cause, Effect, EffectData, Exit, Layer, Or, Schedule, Schema, caps, effect, err, fail,
  from_async, hlist, need, sleep, succeed,
};
pub use id_effect_cli::{RunMainConfig, run_main};
pub use id_effect_config::Secret;

#[cfg(feature = "web")]
pub use id_effect_axum::metrics::metrics_router;
#[cfg(feature = "web")]
pub use id_effect_axum::{
  EffectExecutor, SecurityLayer, execute, health_router, observability_routes, readiness_router,
  routing, serve_until_shutdown,
};

#[cfg(feature = "sql-pg")]
pub use id_effect_sql::{SqlClientService, SqlError, with_transaction};

#[cfg(feature = "otel")]
pub use id_effect_opentelemetry::{OtelConfig, install_from_config};
