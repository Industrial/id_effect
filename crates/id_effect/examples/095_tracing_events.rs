//! Ex 095 — Emit structured effect and fiber events into the tracing buffer.
use id_effect::next::{
  Effect, EffectEvent, FiberEvent, TracingConfig, emit_effect_event, emit_fiber_event,
  install_tracing_layer, snapshot_tracing, succeed,
};
use id_effect::{Exit, Never};

fn main() {
  install_tracing_layer(TracingConfig::enabled());
  let eff: Effect<u8, Never> = emit_effect_event(EffectEvent::Start {
    span: "job".to_owned(),
  })
  .then(emit_fiber_event(FiberEvent::Spawn {
    fiber_id: "fiber-x".to_owned(),
  }))
  .then(succeed(1));
  assert_eq!(eff.block_on(), Exit::succeed(1));
  let snap = snapshot_tracing();
  assert!(
    snap
      .fiber_events
      .iter()
      .any(|e| matches!(e, FiberEvent::Spawn { .. }))
  );
  println!("095_tracing_events ok");
}
