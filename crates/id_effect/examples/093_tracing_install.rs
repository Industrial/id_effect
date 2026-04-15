//! Ex 093 — Install the in-memory tracing layer (test / example backend).
use id_effect::{TracingConfig, install_tracing_layer, run_blocking};

fn main() {
  assert_eq!(
    run_blocking(install_tracing_layer(TracingConfig::enabled()), ()),
    Ok(())
  );
  println!("093_tracing_install ok");
}
