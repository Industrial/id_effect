//! **Stratum 15 — Observability**
//!
//! Structured metrics and tracing instrumentation, built from Strata 0–14.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`metric`] | [`Metric`], counters/gauges/histograms | Stratum 14 (`collections::hash_map`), Stratum 10 (`scheduling::duration`), Stratum 2 (`kernel`) |
//! | [`tracing`] | [`TracingConfig`], span/fiber hooks | Stratum 7 (`concurrency::fiber_ref`), Stratum 14 (`collections::hash_map`), Stratum 2 (`kernel`) |

pub mod metric;
pub mod tracing;

pub use metric::{Metric, make as metric_make};
pub use tracing::{
  AnnotateCurrentSpanErr, AnnotateCurrentSpanSuccess, EffectEvent, FiberEvent, LogSpan, SpanRecord,
  TracingConfig, TracingFiberRefs, TracingSnapshot, annotate_current_span, emit_effect_event,
  emit_fiber_event, install_tracing_layer, snapshot_tracing, with_span,
};
