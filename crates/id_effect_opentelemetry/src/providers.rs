//! The installed OpenTelemetry runtime handle as a capability ([`Layer`]).

use std::sync::Arc;

use id_effect::{HCons, HNil, Layer, hlist};

use crate::starter::OtelStarterGuard;

/// OpenTelemetry runtime handle (flush/shutdown + meter access), the capability
/// [`provide_otel_runtime`] provides (`need::<OtelRuntime>()`).
pub type OtelRuntime = Arc<OtelStarterGuard>;

/// The typed environment holding only [`OtelRuntime`] (the output of [`provide_otel_runtime`]).
pub type OtelRuntimeEnv = HCons<OtelRuntime, HNil>;

/// A [`Layer`] providing `guard` as the [`OtelRuntime`] capability, for domain programs that need
/// OTEL flush/shutdown or the meter.
#[inline]
pub fn provide_otel_runtime<E: Send + 'static>(guard: OtelRuntime) -> Layer<OtelRuntimeEnv, E> {
  Layer::succeed(hlist![guard])
}
