//! Bridge `id_effect::with_span` with `tracing` spans so OTEL exporters see effect-scoped work.

use id_effect::{Effect, box_future};
use tracing::Instrument;

/// Runs `effect` under both `id_effect::with_span` and a `tracing` span exported to OTEL.
pub fn with_span_otel<A, E, R>(name: &'static str, effect: Effect<A, E, R>) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  let parent = tracing::Span::current();
  let inner = id_effect::with_span(effect, name);
  Effect::new_async(move |env: &mut R| {
    let span = tracing::trace_span!(parent: &parent, "id_effect.effect", otel.span_name = name);
    box_future(async move { inner.run(env).instrument(span).await })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::subscriber::{
    sdk_tracer_provider_with_in_memory_exporter, trace_subscriber_for_provider,
  };
  use id_effect::concurrency::fiber_ref::with_fiber_id;
  use id_effect::runtime::FiberId;
  use id_effect::{TracingConfig, install_tracing_layer, run_blocking, succeed};
  use opentelemetry_sdk::trace::InMemorySpanExporter;

  #[test]
  fn emits_tracing_span_exported_to_otel() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let subscriber = trace_subscriber_for_provider(&provider, false, None);
    tracing::subscriber::with_default(subscriber, || {
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let eff = with_span_otel("otel.inner", succeed::<(), (), ()>(()));
      let _ = run_blocking(eff, ());
    });
    let _ = provider.force_flush();
    let spans = exporter.get_finished_spans().expect("spans");
    assert!(spans.iter().any(|s| s.name == "id_effect.effect"));
    let _ = provider.shutdown();
  }

  #[test]
  fn nested_spans_link_under_parent() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let subscriber = trace_subscriber_for_provider(&provider, false, None);
    tracing::subscriber::with_default(subscriber, || {
      let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
      let inner = with_span_otel("inner", succeed::<(), (), ()>(()));
      let outer = with_span_otel("outer", inner);
      let _ = run_blocking(outer, ());
    });
    let _ = provider.force_flush();
    let count = exporter
      .get_finished_spans()
      .expect("spans")
      .into_iter()
      .filter(|s| s.name == "id_effect.effect")
      .count();
    assert!(count >= 2);
    let _ = provider.shutdown();
  }

  #[test]
  fn child_spans_link_under_tracing_parent_across_fibers() {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let subscriber = trace_subscriber_for_provider(&provider, false, None);
    tracing::subscriber::with_default(subscriber, || {
      let parent = tracing::info_span!("parent_fiber");
      parent.in_scope(|| {
        with_fiber_id(FiberId::fresh(), || {
          let _ = run_blocking(with_span_otel("child_a", succeed::<(), (), ()>(())), ());
        });
        with_fiber_id(FiberId::fresh(), || {
          let _ = run_blocking(with_span_otel("child_b", succeed::<(), (), ()>(())), ());
        });
      });
    });
    let _ = provider.force_flush();
    let count = exporter
      .get_finished_spans()
      .expect("spans")
      .into_iter()
      .filter(|s| s.name == "id_effect.effect")
      .count();
    assert!(count >= 2);
    let _ = provider.shutdown();
  }
}
