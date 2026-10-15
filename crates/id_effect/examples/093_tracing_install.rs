//! Ex 093 — Install the in-process tracing buffer (a plain function on the 0.7 kernel).
use id_effect::next::{TracingConfig, install_tracing_layer, snapshot_tracing};

fn main() {
  install_tracing_layer(TracingConfig::enabled());
  assert!(snapshot_tracing().spans.is_empty());
  println!("093_tracing_install ok");
}
