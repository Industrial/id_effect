//! Ex 094 — `with_span` wraps work in a logical span; `tracing_hook` records it.
use id_effect::{
  Effect, TracingConfig, install_tracing_layer, snapshot_tracing, succeed, tracing_hook,
};
use id_effect::{Exit, Never};

fn main() {
  install_tracing_layer(TracingConfig::enabled());
  let eff: Effect<u8, Never> = succeed(9);
  let exit = eff
    .with_span("demo.span")
    .with_span_hook(tracing_hook())
    .block_on();
  assert_eq!(exit, Exit::succeed(9));
  assert!(
    snapshot_tracing()
      .spans
      .iter()
      .any(|s| s.name == "demo.span")
  );
  println!("094_tracing_spans ok");
}
