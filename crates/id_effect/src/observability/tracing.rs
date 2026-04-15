//! Lightweight tracing hooks for effect/fiber observability.

use crate::Effect;
use crate::collections::EffectHashMap;
use crate::collections::hash_map;
use crate::concurrency::fiber_ref::FiberRef;
use crate::effect;
use crate::kernel::box_future;
use crate::runtime::{Never, run_blocking};
use std::sync::{Mutex, OnceLock};

mod annotate_current_span_seal {
  pub(super) trait Success {}
  pub(super) trait Error {}
}

/// Success type for [`annotate_current_span`]. Implemented for [`unit`](()) only (sealed) so `A`
/// infers at call sites while the API stays `Effect<A, E, R>`-shaped.
#[allow(private_bounds)] // seal traits in `annotate_current_span_seal` are intentionally private
pub trait AnnotateCurrentSpanSuccess: From<()> + annotate_current_span_seal::Success {}

/// Error type for [`annotate_current_span`]. Implemented for [`Never`] only (sealed); the helper is
/// infallible.
#[allow(private_bounds)]
pub trait AnnotateCurrentSpanErr: From<Never> + annotate_current_span_seal::Error {}

impl annotate_current_span_seal::Success for () {}
impl AnnotateCurrentSpanSuccess for () {}

impl annotate_current_span_seal::Error for Never {}
impl AnnotateCurrentSpanErr for Never {}

/// Global tracing toggle installed by [`install_tracing_layer`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TracingConfig {
  /// When `false`, hooks and span recording are no-ops.
  pub enabled: bool,
}

impl TracingConfig {
  /// Config with tracing turned on.
  #[inline]
  pub fn enabled() -> Self {
    Self { enabled: true }
  }
}

/// Lifecycle markers for effects wrapped in [`with_span`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectEvent {
  /// Entered a named span.
  Start {
    /// Span name (matches [`with_span`] argument).
    span: String,
  },
  /// Effect under the span completed successfully.
  Success {
    /// Span name (matches [`with_span`] argument).
    span: String,
  },
  /// Effect under the span failed.
  Failure {
    /// Span name (matches [`with_span`] argument).
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

/// Aggregated span metadata keyed by span name.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SpanRecord {
  /// Span identifier (same string passed to [`with_span`]).
  pub name: String,
  /// Key–value annotations merged from fiber-local state on span end.
  pub annotations: EffectHashMap<String, String>,
}

/// One frame on the fiber-local span stack (`TracingFiberRefs::span_stack`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogSpan {
  /// Name of the active span frame.
  pub name: String,
}

/// Point-in-time copy of recorded tracing buffers.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TracingSnapshot {
  /// Ordered [`EffectEvent`] stream since last install.
  pub effect_events: Vec<EffectEvent>,
  /// Ordered [`FiberEvent`] stream since last install.
  pub fiber_events: Vec<FiberEvent>,
  /// Span records with merged annotations.
  pub spans: Vec<SpanRecord>,
}

#[derive(Default)]
struct TraceState {
  config: TracingConfig,
  effect_events: Vec<EffectEvent>,
  fiber_events: Vec<FiberEvent>,
  spans: Vec<SpanRecord>,
}

static TRACE_STATE: OnceLock<Mutex<TraceState>> = OnceLock::new();

/// Fiber-local span stack and current-span annotation map (see beads n48f).
#[derive(Clone)]
pub struct TracingFiberRefs {
  /// Current span stack for this fiber.
  pub span_stack: FiberRef<Vec<LogSpan>>,
  /// Mutable annotations for the innermost span.
  pub span_annotations: FiberRef<EffectHashMap<String, String>>,
}

static TRACING_FIBER_REFS: OnceLock<TracingFiberRefs> = OnceLock::new();

fn trace_state() -> &'static Mutex<TraceState> {
  TRACE_STATE.get_or_init(|| Mutex::new(TraceState::default()))
}

fn tracing_enabled_fast() -> bool {
  trace_state().lock().ok().is_some_and(|g| g.config.enabled)
}

pub(crate) fn fiber_refs() -> Option<&'static TracingFiberRefs> {
  TRACING_FIBER_REFS.get()
}

fn with_state_mut<F>(f: F)
where
  F: FnOnce(&mut TraceState),
{
  let mut guard = trace_state().lock().expect("trace state mutex poisoned");
  if !guard.config.enabled {
    return;
  }
  f(&mut guard);
}

fn ensure_span_exists(spans: &mut Vec<SpanRecord>, name: &str) {
  if spans.iter().all(|span| span.name != name) {
    spans.push(SpanRecord {
      name: name.to_owned(),
      annotations: EffectHashMap::new(),
    });
  }
}

/// Installs fiber refs and replaces global trace buffers; clears prior events.
pub fn install_tracing_layer(config: TracingConfig) -> Effect<(), Never, ()> {
  Effect::new(move |_env| {
    TRACING_FIBER_REFS.get_or_init(|| {
      let span_stack = run_blocking(
        FiberRef::make_with(
          Vec::<LogSpan>::new,
          |_parent| Vec::new(),
          |parent, _child| parent.clone(),
        ),
        (),
      )
      .expect("tracing span_stack FiberRef");
      let span_annotations = run_blocking(
        FiberRef::make_with(
          hash_map::empty::<String, String>,
          |_parent| hash_map::empty(),
          |parent, _child| parent.clone(),
        ),
        (),
      )
      .expect("tracing span_annotations FiberRef");
      TracingFiberRefs {
        span_stack,
        span_annotations,
      }
    });
    let mut guard = trace_state().lock().expect("trace state mutex poisoned");
    guard.config = config.clone();
    guard.effect_events.clear();
    guard.fiber_events.clear();
    guard.spans.clear();
    Ok(())
  })
}

/// Appends an effect lifecycle event when tracing is enabled.
pub fn emit_effect_event(event: EffectEvent) -> Effect<(), Never, ()> {
  Effect::new(move |_env| {
    with_state_mut(|state| {
      if let EffectEvent::Start { span } = &event {
        ensure_span_exists(&mut state.spans, span);
      }
      state.effect_events.push(event.clone());
    });
    Ok(())
  })
}

/// Appends a fiber lifecycle event when tracing is enabled.
pub fn emit_fiber_event(event: FiberEvent) -> Effect<(), Never, ()> {
  Effect::new(move |_env| {
    with_state_mut(|state| {
      state.fiber_events.push(event.clone());
    });
    Ok(())
  })
}

/// Adds `key` → `value` to the current span’s annotation map (fiber-local).
///
/// Generic over `A`, `E`, and `R` for composition with other `Effect<A, E, R>` graphs. In practice
/// `A` is [`unit`](()) and `E` is [`Never`] (see [`AnnotateCurrentSpanSuccess`],
/// [`AnnotateCurrentSpanErr`]). The body does not read `R`; nested fiber ops still use
/// [`run_blocking`] with `()` where those effects require it.
///
/// Type inference often needs an explicit turbofish, e.g.
/// `annotate_current_span::<(), Never, ()>(key, value)` or `::<(), Never, R>` under a generic env.
pub fn annotate_current_span<A, E, R>(
  key: &'static str,
  value: impl Into<String>,
) -> Effect<A, E, R>
where
  A: AnnotateCurrentSpanSuccess + 'static,
  E: AnnotateCurrentSpanErr + 'static,
  R: 'static,
{
  let value = value.into();
  effect!(|_r: &mut R| {
    if !tracing_enabled_fast() {
      return Ok(A::from(()));
    }

    let Some(refs) = fiber_refs() else {
      return Ok(A::from(()));
    };

    // FiberRef ops are `Effect<_, _, ()>` — do not `~` them here: `effect!` lowers `~` to `?`, and
    // those results do not convert to a caller-chosen generic `E`. Drive them with `run_blocking`.
    let stack = run_blocking(refs.span_stack.get(), ()).expect("span_stack get");
    if stack.is_empty() {
      return Ok(A::from(()));
    }

    let span_name = stack.last().expect("non-empty span stack").name.clone();
    let val = value.clone();
    run_blocking(
      refs
        .span_annotations
        .update(move |m| hash_map::set(&m, key.to_string(), val)),
      (),
    )
    .expect("span_annotations update");

    with_state_mut(|state| {
      if let Some(span) = state
        .spans
        .iter_mut()
        .rev()
        .find(|span| span.name == span_name)
      {
        span.annotations = hash_map::set(&span.annotations, key.to_string(), value.clone());
      }
    });

    A::from(())
  })
}

/// Runs `effect` inside a named span: pushes stack, emits start/success/failure, flushes annotations.
pub fn with_span<A, E, R>(effect: Effect<A, E, R>, name: &'static str) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  let span_name = name.to_string();
  Effect::new_async(move |env: &mut R| {
    let span_name = span_name.clone();
    box_future(async move {
      if !tracing_enabled_fast() {
        return effect.run(env).await;
      }
      let Some(refs) = fiber_refs().cloned() else {
        return effect.run(env).await;
      };
      let span_name_for_push = span_name.clone();
      run_blocking(
        refs.span_stack.update(|mut v| {
          v.push(LogSpan {
            name: span_name_for_push,
          });
          v
        }),
        (),
      )
      .expect("span_stack push");

      let empty_ann = hash_map::empty::<String, String>();
      let refs_for_inner = refs.clone();
      let span_name_inner = span_name.clone();
      let inner = Effect::new_async(move |env: &mut R| {
        let span_name = span_name_inner.clone();
        let refs = refs_for_inner.clone();
        box_future(async move {
          let _ = emit_effect_event(EffectEvent::Start {
            span: span_name.clone(),
          })
          .run(&mut ())
          .await;
          let out = effect.run(env).await;
          if tracing_enabled_fast() {
            let ann = run_blocking(refs.span_annotations.get(), ())
              .expect("span_annotations get for flush");
            with_state_mut(|state| {
              ensure_span_exists(&mut state.spans, &span_name);
              if let Some(rec) = state.spans.iter_mut().rev().find(|s| s.name == span_name) {
                rec.annotations = ann;
              }
            });
          }
          let event = match &out {
            Ok(_) => EffectEvent::Success {
              span: span_name.clone(),
            },
            Err(_) => EffectEvent::Failure {
              span: span_name.clone(),
            },
          };
          let _ = emit_effect_event(event).run(&mut ()).await;
          out
        })
      });

      let out = refs
        .span_annotations
        .locally(empty_ann, inner)
        .run(env)
        .await;

      run_blocking(
        refs.span_stack.update(|mut v| {
          v.pop();
          v
        }),
        (),
      )
      .expect("span_stack pop");

      out
    })
  })
}

/// Clones the current global trace buffers (lock held briefly).
pub fn snapshot_tracing() -> TracingSnapshot {
  let guard = trace_state().lock().expect("trace state mutex poisoned");
  TracingSnapshot {
    effect_events: guard.effect_events.clone(),
    fiber_events: guard.fiber_events.clone(),
    spans: guard.spans.clone(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::collections::hash_map;
  use crate::{fail, runtime::run_blocking, succeed};
  use rstest::rstest;
  use std::sync::{Mutex, OnceLock};

  static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

  fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
      .get_or_init(|| Mutex::new(()))
      .lock()
      .expect("test lock mutex poisoned")
  }

  mod with_span_events {
    use super::*;

    #[test]
    fn with_span_when_effect_succeeds_records_start_and_success_events() {
      let _guard = test_lock();
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let eff = with_span(succeed::<u32, (), ()>(7), "test.span");
      let out = run_blocking(eff, ());
      assert_eq!(out, Ok(7));

      let snapshot = snapshot_tracing();
      assert_eq!(
        snapshot.effect_events,
        vec![
          EffectEvent::Start {
            span: "test.span".to_string()
          },
          EffectEvent::Success {
            span: "test.span".to_string()
          }
        ]
      );
    }

    #[test]
    fn with_span_when_effect_fails_records_start_and_failure_events() {
      let _guard = test_lock();
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let eff = with_span(fail::<(), &'static str, ()>("boom"), "failure.span");
      let out = run_blocking(eff, ());
      assert_eq!(out, Err("boom"));

      let snapshot = snapshot_tracing();
      assert_eq!(
        snapshot.effect_events,
        vec![
          EffectEvent::Start {
            span: "failure.span".to_string()
          },
          EffectEvent::Failure {
            span: "failure.span".to_string()
          }
        ]
      );
    }
  }

  mod hooks_and_config {
    use super::*;

    #[test]
    fn annotation_and_fiber_event_hooks_when_enabled_record_data() {
      let _guard = test_lock();
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let eff = with_span(
        annotate_current_span::<(), Never, ()>("market", "SOL-PERP").flat_map(|_| {
          emit_fiber_event(FiberEvent::Spawn {
            fiber_id: "fiber-1".to_string(),
          })
        }),
        "annotated.span",
      );
      let _ = run_blocking(eff, ());

      let snapshot = snapshot_tracing();
      assert_eq!(snapshot.fiber_events.len(), 1);
      let span = snapshot
        .spans
        .iter()
        .find(|s| s.name == "annotated.span")
        .expect("span should be present");
      assert_eq!(
        span.annotations.get("market"),
        Some(&"SOL-PERP".to_string())
      );
    }

    #[rstest]
    #[case::effect_event(0)]
    #[case::fiber_event(1)]
    fn emit_hooks_when_tracing_disabled_do_not_record_events(#[case] mode: u8) {
      let _guard = test_lock();
      let _ = run_blocking(install_tracing_layer(TracingConfig { enabled: false }), ());
      if mode == 0 {
        let _ = run_blocking(
          emit_effect_event(EffectEvent::Start {
            span: "disabled.span".to_string(),
          }),
          (),
        );
      } else {
        let _ = run_blocking(
          emit_fiber_event(FiberEvent::Spawn {
            fiber_id: "fiber-disabled".to_string(),
          }),
          (),
        );
      }
      let snapshot = snapshot_tracing();
      assert!(snapshot.effect_events.is_empty());
      assert!(snapshot.fiber_events.is_empty());
      assert!(snapshot.spans.is_empty());
    }

    #[test]
    fn annotate_current_span_when_no_active_span_is_present_is_noop() {
      let _guard = test_lock();
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let _ = run_blocking(annotate_current_span::<(), Never, ()>("k", "v"), ());
      let snapshot = snapshot_tracing();
      assert!(snapshot.spans.is_empty());
      assert!(snapshot.effect_events.is_empty());
      assert!(snapshot.fiber_events.is_empty());
    }

    #[test]
    fn tracing_config_enabled_constructor_sets_enabled_true() {
      let cfg = TracingConfig::enabled();
      assert!(cfg.enabled);
    }

    #[test]
    fn tracing_snapshot_annotations_preserved_across_clone() {
      let _guard = test_lock();
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let eff = with_span(
        annotate_current_span::<(), Never, ()>("market", "SOL-PERP"),
        "clone.span",
      );
      let _ = run_blocking(eff, ());

      let snap = snapshot_tracing();
      let mut snap_clone = snap.clone();
      let span = snap_clone
        .spans
        .iter_mut()
        .find(|s| s.name == "clone.span")
        .expect("span recorded");
      span.annotations = hash_map::set(
        &span.annotations,
        "market".to_string(),
        "edited".to_string(),
      );

      let orig = snap
        .spans
        .iter()
        .find(|s| s.name == "clone.span")
        .expect("span in original snapshot");
      assert_eq!(
        orig.annotations.get("market"),
        Some(&"SOL-PERP".to_string())
      );
      assert_eq!(
        snap_clone
          .spans
          .iter()
          .find(|s| s.name == "clone.span")
          .expect("span in clone")
          .annotations
          .get("market"),
        Some(&"edited".to_string())
      );
    }
  }

  mod fiber_local_tracing {
    use super::*;
    use crate::concurrency::fiber_ref::with_fiber_id;
    use crate::runtime::FiberId;

    #[test]
    fn annotation_isolated_per_fiber() {
      let _guard = test_lock();
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let ef_a = with_span(
        annotate_current_span::<(), Never, ()>("k", "fiber-a"),
        "span.a",
      );
      let ef_b = with_span(
        annotate_current_span::<(), Never, ()>("k", "fiber-b"),
        "span.b",
      );
      with_fiber_id(FiberId::fresh(), || {
        let _ = run_blocking(ef_a, ());
      });
      with_fiber_id(FiberId::fresh(), || {
        let _ = run_blocking(ef_b, ());
      });
      let snap = snapshot_tracing();
      let sa = snap
        .spans
        .iter()
        .find(|s| s.name == "span.a")
        .expect("span.a");
      let sb = snap
        .spans
        .iter()
        .find(|s| s.name == "span.b")
        .expect("span.b");
      assert_eq!(sa.annotations.get("k"), Some(&"fiber-a".to_string()));
      assert_eq!(sb.annotations.get("k"), Some(&"fiber-b".to_string()));
    }

    #[test]
    fn span_stack_not_shared_between_fibers() {
      let _guard = test_lock();
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let refs = fiber_refs().expect("refs").clone();
      let id_a = FiberId::fresh();
      let id_b = FiberId::fresh();
      with_fiber_id(id_a, || {
        run_blocking(
          refs.span_stack.update(|mut v| {
            v.push(LogSpan {
              name: "only-a".into(),
            });
            v
          }),
          (),
        )
        .expect("push stack");
      });
      with_fiber_id(id_b, || {
        let len = run_blocking(refs.span_stack.get(), ())
          .expect("get stack")
          .len();
        assert_eq!(len, 0, "B should not see A's stack");
      });
      with_fiber_id(id_a, || {
        let len = run_blocking(refs.span_stack.get(), ())
          .expect("get stack a")
          .len();
        assert_eq!(len, 1);
      });
    }

    #[test]
    fn with_span_pushes_then_pops() {
      let _guard = test_lock();
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let refs = fiber_refs().expect("refs").clone();
      let eff = with_span(with_span(succeed::<(), (), ()>(()), "inner"), "outer");
      let _ = run_blocking(eff, ());
      let len = run_blocking(refs.span_stack.get(), ())
        .expect("stack len")
        .len();
      assert_eq!(len, 0);
    }
  }
}
