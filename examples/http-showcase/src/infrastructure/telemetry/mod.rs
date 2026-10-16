//! Request telemetry: one OpenTelemetry span and one latency histogram observation per request.
//!
//! [`Telemetry::instrument`] wraps a [`Router`](axum::Router) in a middleware that runs every
//! request (the rest of the stack, handler included) as one fiber under
//!
//! - a span `"{method} {route}"` with the attributes `http.route` (the matched route
//!   template, e.g. `/todos/{id}`, or [`Telemetry::UNMATCHED`]) and `http.request.method`,
//!   exported through an [`OtelSpanHook`] and parented under the incoming W3C `traceparent` when
//!   there is one;
//! - `Histogram::track_duration` on [`Telemetry::REQUEST_DURATION`] (seconds), tagged with the
//!   same two attributes, in a `MetricRegistry` bridged to OpenTelemetry
//!   (`id_effect_opentelemetry::bridge_metrics`).
//!
//! Export is optional. [`Telemetry::from_env`] installs the OTLP starter only when
//! `OTEL_EXPORTER_OTLP_ENDPOINT` ([`Telemetry::ENDPOINT_VAR`]) is set (the other standard
//! `OTEL_*` variables apply, see `OtelConfig::from_env`); without it, or when the exporter
//! cannot be built, spans are not exported and the histogram only records into the process
//! registry, so the showcase runs without a collector.
//!
//! Spans that handlers open themselves (`Effect::with_span`) are not exported: handler effects
//! run on their own request fibers (`id_effect_axum`), outside this middleware's span hook.
//!
//! ```
//! use axum::Router;
//! use axum::routing::get;
//! use id_effect::MetricRegistry;
//! use http_showcase::infrastructure::telemetry::Telemetry;
//!
//! // No exporter: requests are timed into `registry`, spans go nowhere.
//! let registry = MetricRegistry::new();
//! let app: Router = Telemetry::new(&registry, None)
//!   .instrument(Router::new().route("/ping", get(|| async { "pong" })));
//! # let _ = app;
//! ```
//!
//! [`OtelSpanHook`]: id_effect_opentelemetry::OtelSpanHook

mod instrument;
mod remote_parent;
#[allow(clippy::module_inception)]
mod telemetry;
mod track_request;

pub use instrument::instrument;
pub(crate) use remote_parent::remote_parent;
pub use telemetry::Telemetry;
pub(crate) use track_request::track_request;
