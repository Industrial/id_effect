//! Ex 096 — `snapshot_tracing` reads the recorded spans and events.
use id_effect::{
  Effect, EffectEvent, TracingConfig, annotate_current_span, install_tracing_layer,
  snapshot_tracing, succeed, tracing_hook,
};
use id_effect::{Exit, Never};

fn main() {
  install_tracing_layer(TracingConfig::enabled());
  let eff: Effect<u8, Never> = annotate_current_span("k", "v").then(succeed(0));
  let exit = eff
    .with_span("snap.demo")
    .with_span_hook(tracing_hook())
    .block_on();
  assert_eq!(exit, Exit::succeed(0));
  let snap = snapshot_tracing();
  assert!(
    snap
      .effect_events
      .iter()
      .any(|e| matches!(e, EffectEvent::Start { span } if span == "snap.demo"))
  );
  println!("096_tracing_snapshot ok");
}
