# OpenTelemetry (`id_effect_opentelemetry`)

The workspace crate **`id_effect_opentelemetry`** implements **Phase B** of the Effect.ts parity
plan: **OpenTelemetry** traces and metrics for `id_effect`'s spans (`Effect::with_span`) and
metric registry (`MetricRegistry`, `Counter`, `Histogram`, …).

Design goals match [`@effect/opentelemetry`](https://www.npmjs.com/package/@effect/opentelemetry):

- **Opt-in at the dependency boundary** — the core `id_effect` crate does not depend on OTEL.
- **Spans** — a `SpanHook` turns every `with_span` into an OTEL span, parented across fibers.
- **Metrics** — every series of a `MetricRegistry` is exported to an OTEL meter.
- **Propagation** — W3C Trace Context (`traceparent` / `tracestate`) on portable header maps.

## Spans: `OtelSpanHook`

`with_span(name)` alone records nothing in OTEL; run the root effect under the hook. The hook is a
`FiberRef`, so forked fibers on any executor thread export their spans under the right parent. No
`tracing` subscriber is needed for effect spans. From `examples/otel_minimal.rs`:

```rust
use id_effect::{Effect, Exit, MetricRegistry, succeed};
use id_effect_opentelemetry::{
    OtelSpanHook, bridge_metrics, sdk_meter_provider_with_in_memory_exporter,
    sdk_tracer_provider_with_in_memory_exporter,
};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::InMemoryMetricExporter;
use opentelemetry_sdk::trace::InMemorySpanExporter;

let span_exporter = InMemorySpanExporter::default();
let tracer_provider = sdk_tracer_provider_with_in_memory_exporter(&span_exporter);
let metric_exporter = InMemoryMetricExporter::default();
let meter_provider = sdk_meter_provider_with_in_memory_exporter(&metric_exporter);

let registry = MetricRegistry::new();
let _bridge = bridge_metrics(&registry, meter_provider.meter("example")); // lives as long as `_bridge`
let work = registry.counter("example.work");

let hook = OtelSpanHook::for_provider(&tracer_provider, "example").into_hook();
let eff: Effect<u32, ()> = work
    .increment()
    .then(succeed(42))
    .with_span("example.work")
    .with_span_hook(hook);
assert!(matches!(eff.block_on(), Exit::Success(42)));
```

Span status: success is `Ok`, a typed failure or a defect is `Error`, an interrupt leaves the
status unset with `id_effect.interrupted = true`.

## Metrics: `bridge_metrics`

`bridge_metrics(&registry, meter)` (or `bridge_global_metrics(meter)` for
`MetricRegistry::global()`) exports every series: counters and gauges as observable instruments
read at each collection, histograms and summaries as `f64` histograms, frequencies as counters with
a `value` attribute. Tags (including fiber tags from `Effect::tag_metrics`) become attributes.

**Cardinality:** keep label sets small and stable; prefer bounded `service`, `route`, `tenant` keys
over high-cardinality user IDs.

## W3C Trace Context on header maps

- `install_w3c_trace_context_propagator()` once per process.
- `inject_trace_context_into_headers(&cx, &mut headers)` before a downstream request; inside an
  effect, `current_otel_context()` is the OTEL `Context` of the current span.
- `extract_trace_context_from_headers(&base, &headers)` for an incoming request; start the request's
  trace from it with `OtelSpanHook::for_provider(..).with_parent(cx)`.
- With the `platform` feature: `inject_into_http_request` / `extract_from_http_request` on
  `id_effect_platform::http::HttpRequest`.

## Production OTLP

Enable the `otlp` feature. At process start (`examples/otel_production.rs`):

```rust
use id_effect::{Counter, Effect, succeed};
use id_effect_opentelemetry::{OtelConfig, install_from_config};
use id_effect_tokio::TokioRuntime;

let guard = install_from_config(OtelConfig::from_env()?)?; // bridges MetricRegistry::global()
let eff: Effect<(), ()> = Counter::new("production.work")
    .increment()
    .then(succeed(()))
    .with_span("production.work")
    .with_span_hook(guard.span_hook());
let _ = TokioRuntime::current()?.run(eff).await;
guard.force_flush();
guard.shutdown();
```

`install_otel_starter(&providers, &config)` is the lower-level form; both install global providers,
W3C propagation and a `tracing_subscriber` stack with trace and log layers (so `tracing` events
such as axum's `trace_request` are exported too). `OtelStarterGuard` exposes `meter()`,
`span_hook()`, `metrics_bridge()`, `force_flush()` and `shutdown()`; `graceful_otel_shutdown(guard)`
and `shutdown_otel_on_signal(guard).await` cover process exit.

### As a capability

`provide_otel_runtime::<E>(Arc::new(guard))` is a `Layer<OtelRuntimeEnv, E>`; domain code that must
flush reads it with `need::<OtelRuntime>()`.

### Logs

`OtelLogBackend` is an `id_effect_logger` `LogBackend` ([Logging](./ch07-11-logger.md)); the
logger's per-thread install caveat applies.

### `id_effect_config` keys (`config` feature)

| Key | Maps to |
|-----|---------|
| `otel.endpoint` | OTLP endpoint |
| `otel.protocol` | `grpc` or `http` |
| `otel.service_name` | `service.name` |
| `otel.service_version` | `service.version` |
| `otel.headers` | OTLP request headers |

`load_otel_config(&provider)` reads them from a [`ConfigProvider`](./ch07-10-config.md), falling
back to `OtelConfig::from_env()`.

See also `examples/http-showcase` (`telemetry.rs`) for an Axum service wired end to end.
