# Observability and health

Part VI observability centers on [`id_effect_opentelemetry`](https://github.com/Industrial/id_effect/tree/main/crates/id_effect_opentelemetry) and Axum health routes in [`id_effect_axum`](https://github.com/Industrial/id_effect/tree/main/crates/id_effect_axum).

## Starter

Install the OpenTelemetry providers once at process startup: `install_otel_starter(&providers, &config)` registers traces, metrics, logs and the `tracing` subscriber, and bridges `MetricRegistry::global()`. With the `otlp` feature, `install_from_config(OtelConfig::from_env()?)` builds OTLP providers from the environment in one call. Keep the returned guard alive.

```rust,no_run
use id_effect::{Counter, Effect, Metric, succeed};
use id_effect_opentelemetry::{OtelConfig, install_from_config};

# fn demo() -> Result<(), Box<dyn std::error::Error>> {
let guard = install_from_config(OtelConfig::from_env()?)?;

let requests = Counter::new("requests").tagged("route", "/users");
let handle: Effect<u32, ()> = requests
    .track_all(1, succeed(42))
    .with_span("handle")
    .with_span_hook(guard.span_hook()); // every `with_span` becomes an OTEL span
# Ok(()) }
```

- **Spans need the hook.** `Effect::with_span` alone records nothing in OTEL: run the root effect under `with_span_hook(guard.span_hook())` or `OtelSpanHook::for_provider(&provider, "svc").into_hook()`. The hook is a `FiberRef`, so forked fibers export under the right parent.
- **Metrics** go through the core `MetricRegistry`: `bridge_metrics(&registry, meter)` / `bridge_global_metrics(meter)` export every series.
- **Tests:** build in-memory providers with `sdk_tracer_provider_with_in_memory_exporter` / `sdk_meter_provider_with_in_memory_exporter` (`OtelProviders::with_in_memory_exporters`), or use `with_otel_test_harness`. See `cargo run -p id_effect_opentelemetry --example otel_minimal`.

## Health and readiness

Mount [`id_effect_axum::health::observability_routes_with_state`](https://docs.rs/id_effect_axum/latest/id_effect_axum/health/fn.observability_routes_with_state.html) on your router:

```rust
use id_effect_axum::health::{ReadinessState, observability_routes_with_state};

let ready = ReadinessState::new(false);
let obs = observability_routes_with_state(ready.clone());
// flip to true after DB pool warm-up
ready.set_ready(true);
```

## Axum trace middleware

`id_effect_opentelemetry::trace_request` extracts W3C `traceparent` and creates a server `tracing` span:

```rust,ignore
use axum::middleware;
use id_effect_opentelemetry::trace_request;

let app = router.layer(middleware::from_fn(trace_request));
```

To parent a handler's effect spans under the incoming trace, start its hook from the headers: `OtelSpanHook::for_provider(..).with_parent(extract_trace_context_from_headers(..))`.

Pair with [`id_effect_rpc::span`](https://github.com/Industrial/id_effect/blob/main/labs/id_effect_rpc/src/span.rs) helpers for RPC-shaped routes (`id_effect_rpc` is an unpublished labs crate; see [API boundaries](./ch29-00-api-boundaries.md)).

## See also

- Part II: [OpenTelemetry chapter](../part2/ch07-12-opentelemetry.md)
- Mission: `platform-observability`
