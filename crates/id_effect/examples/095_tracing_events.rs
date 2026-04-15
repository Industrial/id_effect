//! Ex 095 — Emit structured effect + fiber events into the tracing buffer.
use id_effect::{
  EffectEvent, FiberEvent, Never, TracingConfig, emit_effect_event, emit_fiber_event,
  install_tracing_layer, run_blocking, snapshot_tracing, succeed,
};

fn main() {
  let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
  let eff = emit_effect_event(EffectEvent::Start {
    span: "job".to_owned(),
  })
  .flat_map(|_| {
    emit_fiber_event(FiberEvent::Spawn {
      fiber_id: "fiber-x".to_owned(),
    })
  })
  .flat_map(|_| succeed::<u8, Never, ()>(1));
  assert_eq!(run_blocking(eff, ()), Ok(1));
  let snap = snapshot_tracing();
  assert!(
    snap
      .fiber_events
      .iter()
      .any(|e| matches!(e, FiberEvent::Spawn { .. }))
  );
  println!("095_tracing_events ok");
}
