//! Ex 096 — `snapshot_tracing` reads recorded spans and events.
use id_effect::{
  EffectEvent, Never, TracingConfig, annotate_current_span, install_tracing_layer, run_blocking,
  snapshot_tracing, succeed, with_span,
};

fn main() {
  let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
  let eff = with_span(
    annotate_current_span::<(), Never, ()>("k", "v").flat_map(|_| succeed::<u8, Never, ()>(0)),
    "snap.demo",
  );
  assert_eq!(run_blocking(eff, ()), Ok(0));
  let snap = snapshot_tracing();
  assert!(
    snap
      .effect_events
      .iter()
      .any(|e| matches!(e, EffectEvent::Start { span } if span == "snap.demo"))
  );
  println!("096_tracing_snapshot ok");
}
