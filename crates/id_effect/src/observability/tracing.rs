//! Lightweight tracing hooks for effect/fiber observability.

use crate::Effect;
use crate::concurrency::fiber_ref::FiberRef;
use crate::effect;
use crate::kernel::box_future;
use crate::runtime::{Never, run_blocking};
use std::collections::HashMap;
use std::sync::OnceLock;

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

// The buffer, its config and event types are kernel-neutral and live in `next::observability`
// (`k-retire-v1`); this module keeps only the 0.6 `Effect` wrappers around them.
pub use crate::next::observability::tracing::{
  ComputeEvent, EffectEvent, FiberEvent, SpanRecord, TracingConfig, TracingSnapshot,
  record_compute_event, snapshot_tracing,
};
use crate::next::observability::tracing::{
  record_effect_event, record_fiber_event, record_span_annotation, record_span_annotations,
  tracing_enabled,
};

/// One frame on the fiber-local span stack (`TracingFiberRefs::span_stack`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogSpan {
  /// Name of the active span frame.
  pub name: String,
}

/// Fiber-local span stack and current-span annotation map (see beads n48f).
#[derive(Clone)]
pub struct TracingFiberRefs {
  /// Current span stack for this fiber.
  pub span_stack: FiberRef<Vec<LogSpan>>,
  /// Mutable annotations for the innermost span.
  pub span_annotations: FiberRef<HashMap<String, String>>,
}

static TRACING_FIBER_REFS: OnceLock<TracingFiberRefs> = OnceLock::new();

fn tracing_enabled_fast() -> bool {
  tracing_enabled()
}

pub(crate) fn fiber_refs() -> Option<&'static TracingFiberRefs> {
  TRACING_FIBER_REFS.get()
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
          HashMap::<String, String>::new,
          |_parent| HashMap::new(),
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
    crate::next::observability::tracing::install_tracing_layer(config.clone());
    Ok(())
  })
}

/// Appends an effect lifecycle event when tracing is enabled.
pub fn emit_effect_event(event: EffectEvent) -> Effect<(), Never, ()> {
  Effect::new(move |_env| {
    record_effect_event(event.clone());
    Ok(())
  })
}

/// Appends a fiber lifecycle event when tracing is enabled.
pub fn emit_fiber_event(event: FiberEvent) -> Effect<(), Never, ()> {
  Effect::new(move |_env| {
    record_fiber_event(event.clone());
    Ok(())
  })
}

/// Appends a compute fabric event when tracing is enabled.
pub fn emit_compute_event(event: ComputeEvent) -> Effect<(), Never, ()> {
  Effect::new(move |_env| {
    record_compute_event(event.clone());
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
      refs.span_annotations.update(move |mut m| {
        m.insert(key.to_string(), val);
        m
      }),
      (),
    )
    .expect("span_annotations update");

    record_span_annotation(&span_name, key, value.clone());

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

      let empty_ann = HashMap::<String, String>::new();
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
            record_span_annotations(&span_name, ann);
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{fail, runtime::run_blocking, succeed};
  // The buffer is shared with `next::observability::tracing`: serialize on its lock.
  use crate::next::observability::tracing::test_lock;
  use rstest::rstest;

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
      span
        .annotations
        .insert("market".to_string(), "edited".to_string());

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
