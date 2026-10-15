//! Production-style OTLP setup from environment variables.
//!
//! Requires a running OTLP collector (e.g. Jaeger all-in-one on port 4317).
//!
//! ```bash
//! export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
//! export OTEL_SERVICE_NAME=id_effect_demo
//! cargo run -p id_effect_opentelemetry --features otlp,platform --example otel_production
//! ```

use id_effect::{Counter, Effect, succeed};
use id_effect_opentelemetry::{OtelConfig, install_from_config};
use id_effect_tokio::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let config = OtelConfig::from_env()?;
  let guard = install_from_config(config)?;
  // The guard bridges `MetricRegistry::global()`; its span hook exports `with_span` spans.
  let eff: Effect<(), ()> = Counter::new("production.work")
    .increment()
    .then(succeed(()))
    .with_span("production.work")
    .with_span_hook(guard.span_hook());
  let _ = TokioRuntime::current()?.run(eff).await;
  guard.force_flush();
  println!("exported via OTLP; shutting down");
  guard.shutdown();
  Ok(())
}
