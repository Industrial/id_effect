# OpenTelemetry (`id_effect_opentelemetry`)

The workspace crate **`id_effect_opentelemetry`** implements **Phase B** of the Effect.ts parity plan:
first-class **OpenTelemetry** traces and metrics alongside `id_effect`’s fiber-local
[`with_span`](https://docs.rs/id_effect/latest/id_effect/fn.with_span.html) and
[`Metric`](https://docs.rs/id_effect/latest/id_effect/struct.Metric.html) primitives.

Design goals match [`@effect/opentelemetry`](https://www.npmjs.com/package/@effect/opentelemetry):

- **Opt-in at the dependency boundary** — the core `id_effect` crate does not depend on OTEL.
- **Tracing** — bridge `tracing` spans to OTEL exporters (OTLP in production; in-memory in tests).
- **Propagation** — W3C Trace Context (`traceparent` / `tracestate`) on portable header maps.
- **Metrics** — dual-write bridges from `Metric` counters and duration histograms to OTEL instruments.

## Tracing: compose `with_span` and OTEL

Use [`id_effect_opentelemetry::with_span_otel`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.with_span_otel.html)
when you want **both**:

1. Fiber-local span stack and `EffectEvent` stream (`id_effect::with_span`), and
2. A `tracing` span exported through [`tracing_opentelemetry`](https://docs.rs/tracing-opentelemetry).

Build a tracer provider (here: in-memory for tests), install a `tracing` subscriber with an OTEL layer,
then run effects under `with_span_otel`:

```rust
use id_effect::{install_tracing_layer, run_blocking, succeed, TracingConfig};
use id_effect_opentelemetry::{
  sdk_tracer_provider_with_in_memory_exporter, trace_subscriber_for_provider, with_span_otel,
};
use opentelemetry_sdk::trace::InMemorySpanExporter;

let exporter = InMemorySpanExporter::default();
let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
let subscriber = trace_subscriber_for_provider(&provider, false, None);

tracing::subscriber::with_default(subscriber, || {
  let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());
  let eff = with_span_otel("request", succeed::<(), (), ()>(()));
  let _ = run_blocking(eff, ());
});

let _ = provider.force_flush();
# let _ = provider.shutdown();
```

In production you typically:

1. Configure an OTLP exporter (or another `SpanExporter`) on [`SdkTracerProvider`](https://docs.rs/opentelemetry_sdk/latest/opentelemetry_sdk/trace/struct.SdkTracerProvider.html).
2. Call [`register_global_tracer_provider`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.register_global_tracer_provider.html) once at startup.
3. Install a global [`tracing_subscriber`](https://docs.rs/tracing-subscriber) stack with
   [`tracing_opentelemetry::layer()`](https://docs.rs/tracing-opentelemetry/latest/tracing_opentelemetry/fn.layer.html).

## W3C Trace Context on header maps

For HTTP clients built on `Vec<(String, String)>` (or similar), use:

- [`install_w3c_trace_context_propagator`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.install_w3c_trace_context_propagator.html) once per process.
- [`inject_trace_context_into_headers`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.inject_trace_context_into_headers.html) before sending a downstream request.
- [`extract_trace_context_from_headers`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.extract_trace_context_from_headers.html) when handling an incoming request.

For Axum / `http::HeaderMap`, map headers into this shape at the boundary or add a small adapter in your app.

## Metrics: `CounterBridge` and `DurationHistogramBridge`

[`CounterBridge`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/struct.CounterBridge.html) and
[`DurationHistogramBridge`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/struct.DurationHistogramBridge.html)
keep `id_effect::Metric` snapshots for tests while exporting measurements to OTEL.

Create an [`SdkMeterProvider`](https://docs.rs/opentelemetry_sdk/latest/opentelemetry_sdk/metrics/struct.SdkMeterProvider.html)
with a [`PeriodicReader`](https://docs.rs/opentelemetry_sdk/latest/opentelemetry_sdk/metrics/struct.PeriodicReader.html)
and an [`InMemoryMetricExporter`](https://docs.rs/opentelemetry_sdk/latest/opentelemetry_sdk/metrics/struct.InMemoryMetricExporter.html)
for CI-style assertions, or wire an OTLP metrics exporter for production.

**Cardinality:** keep label sets small and stable; prefer bounded `service`, `route`, `tenant` keys over
high-cardinality user IDs in metric attributes.

## Axum + Tokio production sketch

1. Build tracer + meter providers with OTLP endpoints from `id_effect_config` (or env).
2. Register globals and `try_init` a `tracing_subscriber` registry (fmt + OTEL + optional `EnvFilter`).
3. In Axum middleware, extract `traceparent` into an OTEL [`Context`](https://docs.rs/opentelemetry/latest/opentelemetry/struct.Context.html),
   attach it to the request extension, and spawn handler work inside that context.
4. On graceful shutdown, call `force_flush` / `shutdown` on providers (mirror `Scope` finalizer patterns from `id_effect`).

See also: `docs/effect-ts-parity/phases/phase-b-opentelemetry.md` in the repository for the full task breakdown and Beads slug map.


## Production OTLP

Enable the default `otlp` feature (or pass `--features otlp` explicitly). At process start:

```rust
use id_effect_opentelemetry::{OtelConfig, install_from_config, graceful_otel_shutdown};

let guard = install_from_config(OtelConfig::from_env()?)?;
// register Axum routes, run effects, etc.
guard.force_flush();
graceful_otel_shutdown(guard);
```

[`OtelStarterGuard`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/struct.OtelStarterGuard.html) exposes
[`meter()`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/struct.OtelStarterGuard.html#method.meter),
[`force_flush`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/struct.OtelStarterGuard.html#method.force_flush),
and [`shutdown`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/struct.OtelStarterGuard.html#method.shutdown).

### Capability DI

Register the runtime handle for programs that need flush/shutdown in domain code:

```rust
use std::sync::Arc;
use id_effect_opentelemetry::{provide_otel_runtime, OtelRuntime};

let guard = Arc::new(install_from_config(OtelConfig::from_env()?)?);
let provider = provide_otel_runtime(guard);
// env.insert via provider.build(...)
```

### Graceful shutdown (Tokio)

```rust
use id_effect_opentelemetry::shutdown_otel_on_signal;

shutdown_otel_on_signal(guard).await;
```

### Logs

[`install_otel_starter`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.install_otel_starter.html) installs a
`tracing_subscriber` stack with both OTEL trace and log export layers. Use structured
`tracing::info!(target: "id_effect_opentelemetry", ...)` events or wire
[`OtelLogBackend`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/struct.OtelLogBackend.html) into
[`id_effect_logger`](https://docs.rs/id_effect_logger).

### `id_effect_config` keys (`config` feature)

| Key | Maps to |
|-----|---------|
| `otel.endpoint` | OTLP endpoint |
| `otel.protocol` | `grpc` or `http` |
| `otel.service_name` | `service.name` |
| `otel.service_version` | `service.version` |
| `otel.headers` | OTLP request headers |

Use [`load_otel_config`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.load_otel_config.html) when a
[`ConfigProvider`](https://docs.rs/id_effect_config) is already available.
