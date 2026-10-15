# `id_effect_opentelemetry`: metrics through the registry, spans through a span hook

0.7 moves `id_effect_opentelemetry` to the new kernel (`id_effect::next`). The OTLP, config,
propagation, subscriber, logs and shutdown APIs are unchanged. What changed is how effects reach
OpenTelemetry:

| item | 0.6 | 0.7 |
|---|---|---|
| metrics | `CounterBridge` / `DurationHistogramBridge`: dual-write one v1 `Metric` and one OTEL instrument per call | `bridge_metrics(&registry, meter)` / `bridge_global_metrics(meter)`: every series of a `next::MetricRegistry` is exported (removed: `CounterBridge`, `DurationHistogramBridge`) |
| spans | `with_span_otel(name, eff)`: v1 `with_span` + a `tracing` span (needed `install_tracing_layer`) | `OtelSpanHook`: a `next::SpanHook` installed with `eff.with_span_hook(..)`; every `eff.with_span(name)` becomes an OTEL span (removed: `with_span_otel`) |
| starter | `install_otel_starter` installs globals + subscriber | also bridges `MetricRegistry::global()` (kept alive by the guard); `guard.span_hook()` returns the hook; `guard.metrics_bridge()` |
| capability | `provide_otel_runtime(guard) -> ProviderBox` (v1 capability graph) | `provide_otel_runtime::<E>(guard) -> Layer<OtelRuntimeEnv, E>`; `need::<OtelRuntime>()` reads it |
| new | – | `current_otel_context()` (effect) / `span_otel_context(&span)`: the OTEL `Context` of the current `next` span, for W3C injection; `sdk_meter_provider_with_in_memory_exporter` is now public |

```rust
// 0.6
let meter = guard.meter();
let requests = CounterBridge::new(Metric::counter("requests", tags), &meter, "requests");
let eff = with_span_otel("handle", requests.apply(1).flat_map(|_| work()));
run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
run_blocking(eff, ());

// 0.7
use id_effect::next::{Counter, Effect};
let guard = install_otel_starter(&providers, &config)?; // bridges MetricRegistry::global()
let requests = Counter::new("requests").tagged("route", "/users");
let eff: Effect<Out, Err> = requests
  .track_all(1, work())
  .with_span("handle")
  .with_span_hook(guard.span_hook());
TokioRuntime::current()?.run(eff).await;
```

- **Metrics mapping.** Counters become `u64` observable counters and gauges `i64` observable
  gauges, read from the registry at each collection (pull). Histograms become `f64` histograms
  with the registry's bucket boundaries, summaries `f64` histograms (default buckets), and
  frequencies `u64` counters with a `value` attribute; these are pushed on each record by a
  `MetricListener`. Tags, including fiber tags from `Effect::tag_metrics`, become attributes.
  The OTEL instrument name is the metric name.
- **Bridge lifetime.** The bridge lives as long as the returned `MetricsBridge`; dropping it
  detaches it. A registry series created before the bridge is attached is replayed (counter and
  gauge readings are complete; pushed kinds see samples recorded after attaching).
- **Spans need the hook.** `with_span` alone records nothing in OTEL; run the root effect under
  `with_span_hook(OtelSpanHook::for_provider(&provider, "svc").into_hook())` or
  `guard.span_hook()`. The hook is a `FiberRef`, so forked fibers on any executor thread export
  their spans under the right parent. For a request handler, start the trace from incoming W3C
  headers with `OtelSpanHook::for_provider(..).with_parent(extract_trace_context_from_headers(..))`.
- **Span status.** Success is `Ok`, a typed failure or a defect is `Error`, an interrupt leaves
  the status unset with `id_effect.interrupted = true`.
- **No `tracing` subscriber needed for effect spans**, and no `install_tracing_layer`: the
  bridge calls the OTEL tracer directly. `tracing` spans and events (axum `trace_request`,
  `tracing::info!`) still go through the subscriber layers as in 0.6, but are not parented
  under `next` spans.
- **Logs.** `OtelLogBackend` is unchanged. It is an `id_effect_logger` `LogBackend` value and does
  not depend on a thread-local install; when you install it through `id_effect_logger`'s
  thread-local `install_*` API, the 0.7 logger caveat applies (fibers on tokio worker threads
  see only what those threads installed and fall back to `tracing`).
