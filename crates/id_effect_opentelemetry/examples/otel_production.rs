//! Production-style OTLP setup from environment variables.
//!
//! Requires a running OTLP collector (e.g. Jaeger all-in-one on port 4317).
//!
//! ```bash
//! export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
//! export OTEL_SERVICE_NAME=id_effect_demo
//! cargo run -p id_effect_opentelemetry --features otlp,platform --example otel_production
//! ```

use id_effect::{TracingConfig, install_tracing_layer, run_blocking, succeed};
use id_effect_opentelemetry::{OtelConfig, install_from_config, with_span_otel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let config = OtelConfig::from_env()?;
  let guard = install_from_config(config)?;
  let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
  let eff = with_span_otel("production.work", succeed::<(), (), ()>(()));
  let _ = run_blocking(eff, ());
  guard.force_flush();
  println!("exported via OTLP; shutting down");
  guard.shutdown();
  Ok(())
}
