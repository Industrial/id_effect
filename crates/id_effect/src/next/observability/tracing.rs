//! In-process tracing buffer: the 0.6 `install_tracing_layer` / `snapshot_tracing` API on the
//! `next` span hook surface (`k-retire-v1`).
//!
//! The buffer is process-global (one per process, like 0.6): [`install_tracing_layer`] turns it on
//! (or off) and clears it, [`snapshot_tracing`] copies it out. Spans reach it through
//! [`TracingHook`], a [`SpanHook`] installed around the program with
//! [`Effect::with_span_hook`]`(`[`tracing_hook`]`())` (or `LocalEffect::with_span_hook`): every
//! [`Effect::with_span`] inside then records an [`EffectEvent::Start`] and an
//! [`EffectEvent::Success`] / [`EffectEvent::Failure`] (a defect or an interrupt counts as a
//! failure) plus a [`SpanRecord`] with the span's attributes and the annotations added by
//! [`annotate_current_span`]. Fiber and compute-fabric events are appended explicitly with
//! [`emit_fiber_event`] / [`emit_compute_event`] (or the synchronous `record_*` functions).
//!
//! While the buffer is disabled (the default, or after `install_tracing_layer` with
//! `enabled: false`) every hook and `record_*` call is a no-op.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect::next::{
//!   Effect, EffectEvent, TracingConfig, annotate_current_span, install_tracing_layer,
//!   snapshot_tracing, succeed, tracing_hook,
//! };
//!
//! install_tracing_layer(TracingConfig::enabled());
//! let eff: Effect<u8, ()> = annotate_current_span("user", "ada")
//!   .then(succeed(7))
//!   .with_span("request")
//!   .with_span_hook(tracing_hook());
//! assert!(matches!(eff.block_on(), Exit::Success(7)));
//!
//! let snap = snapshot_tracing();
//! assert_eq!(snap.effect_events[0], EffectEvent::Start { span: "request".into() });
//! let span = snap.spans.iter().find(|s| s.name == "request").unwrap();
//! assert_eq!(span.annotations.get("user").map(String::as_str), Some("ada"));
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use super::{Span, SpanHook};
use crate::next::kernel::{Effect, ExitKind, sync};

/// Global switch for the tracing buffer, set by [`install_tracing_layer`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TracingConfig {
  /// When `false`, the hook and every `record_*` / `emit_*` call are no-ops.
  pub enabled: bool,
}

impl TracingConfig {
  /// Config with tracing turned on.
  #[inline]
  pub fn enabled() -> Self {
    Self { enabled: true }
  }
}

/// Lifecycle markers of spans ([`Effect::with_span`] under [`TracingHook`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectEvent {
  /// Entered a named span.
  Start {
    /// Span name.
    span: String,
  },
  /// The effect under the span succeeded.
  Success {
    /// Span name.
    span: String,
  },
  /// The effect under the span failed (typed failure, defect or interrupt).
  Failure {
    /// Span name.
    span: String,
  },
}

/// Coarse fiber lifecycle signals (opt-in via [`emit_fiber_event`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FiberEvent {
  /// Fiber started work.
  Spawn {
    /// Opaque fiber identifier string.
    fiber_id: String,
  },
  /// Fiber finished normally.
  Complete {
    /// Opaque fiber identifier string.
    fiber_id: String,
  },
  /// Fiber was interrupted.
  Interrupt {
    /// Opaque fiber identifier string.
    fiber_id: String,
  },
}

/// Compute Fabric supervisor signals (opt-in via [`record_compute_event`] /
/// [`emit_compute_event`]).
#[derive(Clone, Debug, PartialEq)]
pub enum ComputeEvent {
  /// Supervisor control-loop tick with live telemetry and admission state.
  SupervisorTick {
    /// Process CPU utilization fraction.
    cpu_pct: f32,
    /// Process memory utilization fraction.
    mem_pct: f32,
    /// Admission permits after rebalance.
    admission_permits: usize,
    /// Auto-parallel element threshold for this tick.
    parallelism_threshold: usize,
  },
  /// Admission was throttled to `permits` concurrent workers.
  AdmissionThrottle {
    /// Remaining admission permits.
    permits: usize,
  },
  /// Local fabric offloaded work to the cluster queue.
  ScaleOut {
    /// Logical job / handler name.
    job_name: String,
  },
}

/// Aggregated span metadata, one record per span name.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SpanRecord {
  /// Span name.
  pub name: String,
  /// The span's attributes merged with the annotations added while it ran.
  pub annotations: HashMap<String, String>,
}

/// Point-in-time copy of the tracing buffer.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TracingSnapshot {
  /// Ordered [`EffectEvent`] stream since the last install.
  pub effect_events: Vec<EffectEvent>,
  /// Ordered [`FiberEvent`] stream since the last install.
  pub fiber_events: Vec<FiberEvent>,
  /// Ordered [`ComputeEvent`] stream since the last install.
  pub compute_events: Vec<ComputeEvent>,
  /// Span records with merged annotations.
  pub spans: Vec<SpanRecord>,
}

#[derive(Default)]
struct TraceState {
  config: TracingConfig,
  snapshot: TracingSnapshot,
}

fn state() -> MutexGuard<'static, TraceState> {
  static STATE: OnceLock<Mutex<TraceState>> = OnceLock::new();
  STATE
    .get_or_init(|| Mutex::new(TraceState::default()))
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Runs `f` on the buffer when tracing is enabled.
fn with_buffer(f: impl FnOnce(&mut TracingSnapshot)) {
  let mut guard = state();
  if guard.config.enabled {
    f(&mut guard.snapshot);
  }
}

/// The last record named `name`, created when missing.
fn span_record<'a>(spans: &'a mut Vec<SpanRecord>, name: &str) -> &'a mut SpanRecord {
  match spans.iter().rposition(|s| s.name == name) {
    Some(i) => &mut spans[i],
    None => {
      spans.push(SpanRecord {
        name: name.to_owned(),
        annotations: HashMap::new(),
      });
      spans.last_mut().expect("just pushed")
    }
  }
}

/// Sets the buffer's [`TracingConfig`] and clears every recorded event and span.
pub fn install_tracing_layer(config: TracingConfig) {
  let mut guard = state();
  guard.config = config;
  guard.snapshot = TracingSnapshot::default();
}

/// Whether the buffer is currently recording.
pub fn tracing_enabled() -> bool {
  state().config.enabled
}

/// Copies the current buffer out.
pub fn snapshot_tracing() -> TracingSnapshot {
  state().snapshot.clone()
}

/// Appends `event` (synchronously) when tracing is enabled. A `Start` also creates the span's
/// [`SpanRecord`].
pub fn record_effect_event(event: EffectEvent) {
  with_buffer(|b| {
    if let EffectEvent::Start { span } = &event {
      span_record(&mut b.spans, span);
    }
    b.effect_events.push(event);
  });
}

/// Appends `event` (synchronously) when tracing is enabled.
pub fn record_fiber_event(event: FiberEvent) {
  with_buffer(|b| b.fiber_events.push(event));
}

/// Appends `event` (synchronously) when tracing is enabled (compute supervisor ticks).
pub fn record_compute_event(event: ComputeEvent) {
  with_buffer(|b| b.compute_events.push(event));
}

/// Adds `key = value` to the record of span `name` (created when missing), when tracing is
/// enabled.
pub fn record_span_annotation(name: &str, key: impl Into<String>, value: impl Into<String>) {
  with_buffer(|b| {
    span_record(&mut b.spans, name)
      .annotations
      .insert(key.into(), value.into());
  });
}

/// Replaces the annotations of span `name` (created when missing), when tracing is enabled.
pub(crate) fn record_span_annotations(name: &str, annotations: HashMap<String, String>) {
  with_buffer(|b| span_record(&mut b.spans, name).annotations = annotations);
}

/// [`record_effect_event`] as an effect step.
pub fn emit_effect_event<E: Send + 'static, R: Send + Sync + 'static>(
  event: EffectEvent,
) -> Effect<(), E, R> {
  sync(move || record_effect_event(event))
}

/// [`record_fiber_event`] as an effect step.
pub fn emit_fiber_event<E: Send + 'static, R: Send + Sync + 'static>(
  event: FiberEvent,
) -> Effect<(), E, R> {
  sync(move || record_fiber_event(event))
}

/// [`record_compute_event`] as an effect step.
pub fn emit_compute_event<E: Send + 'static, R: Send + Sync + 'static>(
  event: ComputeEvent,
) -> Effect<(), E, R> {
  sync(move || record_compute_event(event))
}

/// Adds `key = value` to the current span's [`SpanRecord`]; a no-op outside any span or while
/// tracing is disabled.
pub fn annotate_current_span<E: Send + 'static, R: Send + Sync + 'static>(
  key: impl Into<String>,
  value: impl Into<String>,
) -> Effect<(), E, R> {
  let (key, value) = (key.into(), value.into());
  Span::current().map(move |span| {
    if let Some(span) = span {
      record_span_annotation(span.name(), key, value);
    }
  })
}

/// The [`SpanHook`] that records spans into the tracing buffer (see the module docs).
#[derive(Clone, Copy, Debug, Default)]
pub struct TracingHook;

impl SpanHook for TracingHook {
  fn on_start(&self, span: &Span) {
    with_buffer(|b| {
      let rec = span_record(&mut b.spans, span.name());
      rec.annotations.extend(span.attributes().iter().cloned());
      b.effect_events.push(EffectEvent::Start {
        span: span.name().to_owned(),
      });
    });
  }

  fn on_end(&self, span: &Span, _end: Instant, exit: ExitKind) {
    let span = span.name().to_owned();
    record_effect_event(match exit {
      ExitKind::Success => EffectEvent::Success { span },
      _ => EffectEvent::Failure { span },
    });
  }
}

/// The shared [`TracingHook`], ready for [`Effect::with_span_hook`].
pub fn tracing_hook() -> Arc<dyn SpanHook> {
  static HOOK: OnceLock<Arc<dyn SpanHook>> = OnceLock::new();
  Arc::clone(HOOK.get_or_init(|| Arc::new(TracingHook)))
}

/// Serializes the tests (here and in the 0.6 wrappers) that share the process-global buffer.
#[cfg(test)]
pub(crate) fn test_lock() -> MutexGuard<'static, ()> {
  static LOCK: Mutex<()> = Mutex::new(());
  LOCK
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Exit;
  use crate::next::{LocalEffect, die, fail, succeed};

  fn start(span: &str) -> EffectEvent {
    EffectEvent::Start { span: span.into() }
  }

  mod install_tracing_layer {
    use super::*;

    #[test]
    fn clears_the_buffer_and_sets_the_switch() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      record_effect_event(start("a"));
      assert_eq!(snapshot_tracing().effect_events.len(), 1);
      install_tracing_layer(TracingConfig::enabled());
      assert_eq!(snapshot_tracing(), TracingSnapshot::default());
      assert!(tracing_enabled());
    }

    #[test]
    fn disabled_records_nothing() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::default());
      record_effect_event(start("a"));
      record_fiber_event(FiberEvent::Spawn {
        fiber_id: "f".into(),
      });
      record_compute_event(ComputeEvent::ScaleOut {
        job_name: "j".into(),
      });
      record_span_annotation("a", "k", "v");
      let eff: Effect<u8, ()> = succeed(1).with_span("s").with_span_hook(tracing_hook());
      assert!(matches!(eff.block_on(), Exit::Success(1)));
      assert_eq!(snapshot_tracing(), TracingSnapshot::default());
      assert!(!tracing_enabled());
    }
  }

  mod tracing_hook {
    use super::*;

    #[test]
    fn records_start_and_success_with_attributes() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      let eff: Effect<u8, ()> = succeed(7)
        .with_span_attrs("req", [("route", "/health")])
        .with_span_hook(tracing_hook());
      assert!(matches!(eff.block_on(), Exit::Success(7)));
      let snap = snapshot_tracing();
      assert_eq!(
        snap.effect_events,
        vec![start("req"), EffectEvent::Success { span: "req".into() }]
      );
      assert_eq!(
        snap.spans[0].annotations.get("route").map(String::as_str),
        Some("/health")
      );
    }

    #[test]
    fn failure_and_defect_record_failure() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      let failed: Effect<u8, &str> = fail("boom").with_span("f").with_span_hook(tracing_hook());
      let died: Effect<u8, &str> = die("bug").with_span("d").with_span_hook(tracing_hook());
      let _ = failed.block_on();
      let _ = died.block_on();
      let ends: Vec<_> = snapshot_tracing()
        .effect_events
        .into_iter()
        .filter(|e| matches!(e, EffectEvent::Failure { .. }))
        .collect();
      assert_eq!(
        ends,
        vec![
          EffectEvent::Failure { span: "f".into() },
          EffectEvent::Failure { span: "d".into() }
        ]
      );
    }

    #[test]
    fn nested_spans_nest_events() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      let eff: Effect<(), ()> = succeed(())
        .with_span("inner")
        .with_span("outer")
        .with_span_hook(tracing_hook());
      let _ = eff.block_on();
      let names: Vec<String> = snapshot_tracing()
        .effect_events
        .into_iter()
        .map(|e| match e {
          EffectEvent::Start { span } => format!("+{span}"),
          EffectEvent::Success { span } | EffectEvent::Failure { span } => format!("-{span}"),
        })
        .collect();
      assert_eq!(names, ["+outer", "+inner", "-inner", "-outer"]);
    }

    #[test]
    fn without_the_hook_spans_record_nothing() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      let eff: Effect<(), ()> = succeed(()).with_span("quiet");
      let _ = eff.block_on();
      assert!(snapshot_tracing().effect_events.is_empty());
    }

    #[test]
    fn local_effect_with_span_hook_records() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      let inner: Effect<(), ()> = succeed(()).with_span("in-local");
      let eff = LocalEffect::<(), ()>::sync(|| ())
        .then(inner.local())
        .with_span_hook(tracing_hook());
      assert!(matches!(eff.block_on(), Exit::Success(())));
      assert_eq!(snapshot_tracing().effect_events[0], start("in-local"));
    }
  }

  mod annotate_current_span {
    use super::*;

    #[test]
    fn outside_a_span_is_a_noop() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      let eff: Effect<(), ()> = annotate_current_span("k", "v");
      let _ = eff.block_on();
      assert!(snapshot_tracing().spans.is_empty());
    }

    #[test]
    fn annotates_the_innermost_span() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      let eff: Effect<(), ()> = annotate_current_span("k", "inner")
        .with_span("inner")
        .then(annotate_current_span("k", "outer"))
        .with_span("outer")
        .with_span_hook(tracing_hook());
      let _ = eff.block_on();
      let snap = snapshot_tracing();
      let get = |n: &str| {
        snap
          .spans
          .iter()
          .find(|s| s.name == n)
          .and_then(|s| s.annotations.get("k").cloned())
      };
      assert_eq!(get("inner").as_deref(), Some("inner"));
      assert_eq!(get("outer").as_deref(), Some("outer"));
    }
  }

  mod emit {
    use super::*;

    #[test]
    fn emit_steps_append_in_order() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      let eff: Effect<(), ()> = emit_fiber_event(FiberEvent::Spawn {
        fiber_id: "1".into(),
      })
      .then(emit_compute_event(ComputeEvent::AdmissionThrottle {
        permits: 2,
      }))
      .then(emit_effect_event(start("manual")))
      .then(emit_fiber_event(FiberEvent::Complete {
        fiber_id: "1".into(),
      }));
      let _ = eff.block_on();
      let snap = snapshot_tracing();
      assert_eq!(snap.fiber_events.len(), 2);
      assert_eq!(
        snap.compute_events,
        vec![ComputeEvent::AdmissionThrottle { permits: 2 }]
      );
      assert_eq!(snap.effect_events, vec![start("manual")]);
      assert_eq!(snap.spans[0].name, "manual");
    }

    #[test]
    fn record_span_annotations_replaces_the_map() {
      let _g = test_lock();
      install_tracing_layer(TracingConfig::enabled());
      record_span_annotation("s", "a", "1");
      record_span_annotations("s", HashMap::from([("b".to_owned(), "2".to_owned())]));
      let snap = snapshot_tracing();
      assert_eq!(snap.spans.len(), 1);
      assert_eq!(snap.spans[0].annotations.get("a"), None);
      assert_eq!(
        snap.spans[0].annotations.get("b").map(String::as_str),
        Some("2")
      );
    }
  }
}
