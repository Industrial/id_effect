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

```rust
use axum::middleware;
use id_effect_opentelemetry::trace_request;

# let router: axum::Router = axum::Router::new();
let app = router.layer(middleware::from_fn(trace_request));
```

To parent a handler's effect spans under the incoming trace, start its hook from the headers: `OtelSpanHook::for_provider(..).with_parent(extract_trace_context_from_headers(..))`. The middleware enters its span only while the rest of the stack is being polled (`Instrument`), so tasks that run on the same thread while a handler waits never inherit it.

Pair with [`id_effect_rpc::span`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_rpc/src/span.rs) helpers for RPC-shaped routes (`id_effect_rpc` is unpublished until 0.12.0; see [API boundaries](./ch29-00-api-boundaries.md)).

## Outbound calls, database spans and sampling

Client spans are core spans like any other: they reach OTEL through the same span hook, as children of the span that made the call. A span attribute `otel.kind` (`client`, `server`, `producer`, `consumer`) sets the OTEL span kind, which lets crates without an OpenTelemetry dependency emit client spans.

**HTTP.** `TracedHttpClient` (feature `platform`) wraps any `HttpClient`: each call opens a span named after the method (`GET`) with `http.request.method`, `url.full` (user info dropped, signed-URL query values `REDACTED`), `server.address`, `server.port` and `http.response.status_code`, and sends that span's W3C `traceparent`, so the downstream service continues the same trace. A status `>= 400` or an `HttpError` ends the span as an error. Put the traced client outside the retry: `with_retry` does that and records each attempt as an `http.client.attempt` event on the one span, so a call that succeeds on its third try is one span with three events, and every attempt carries the same `traceparent`.

<!-- ignore: book_snippets builds id_effect_opentelemetry without its `platform` feature -->
```rust,ignore
use id_effect_opentelemetry::TracedHttpClient;
use id_effect_platform::http::{ReqwestHttpClient, RetryPolicy, provide_http_client};

let client = TracedHttpClient::with_retry(ReqwestHttpClient::default_client(), RetryPolicy::default());
let http = provide_http_client::<AppError>(client); // the HttpClientService capability
```

**SQL.** `id_effect_sql::metrics::InstrumentedSqlClient` runs every `query` / `execute` in a `db.query` client span with `db.system`, `db.operation` (`SELECT`) and `db.statement`: the SQL text with string and numeric literals replaced by `?`. Bound parameters never reach a span.

```rust
use id_effect::{Effect, Exit};
use id_effect_opentelemetry::{OtelSpanHook, sdk_tracer_provider_with_in_memory_exporter};
use id_effect_sql::metrics::InstrumentedSqlClient;
use id_effect_sql::{SqlClient, SqlError, SqlRow, SqlValue, TestSqlClient};
use opentelemetry_sdk::trace::InMemorySpanExporter;

let exporter = InMemorySpanExporter::default();
let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
let db = TestSqlClient::new();
let sql = "SELECT name FROM users WHERE id = $1";
db.script_query(sql, vec![SqlRow::default()]);
let _ = db.connect().block_on();
let client = InstrumentedSqlClient::new(db).with_db_system("postgresql");

let load: Effect<Vec<SqlRow>, SqlError> = client
    .query(sql, &[SqlValue::I64(7)])
    .with_span("load_user")
    .with_span_hook(OtelSpanHook::for_provider(&provider, "svc").into_hook());
assert!(matches!(load.block_on(), Exit::Success(_)));
let spans = exporter.get_finished_spans().unwrap();
assert!(spans.iter().any(|s| s.name == "db.query"));
```

**Installed where OpenTelemetry is set up.** Both wrappers are decorators: nothing traces a client nobody wrapped. Wrap the capability's layer once, where the app installs telemetry, and every program that reads the `HttpClientService` or `SqlClientService` is traced without touching a call site: `TracedHttpClient::layer(inner)` (feature `platform`) and `instrumented_sql_layer(inner, db_system)` (feature `sql`) take the layer that provides the capability and return one that provides it wrapped (`instrumented_sql_client(service, db_system)` wraps a capability value). With `id_effect_kit`, the `otel` feature brings them as `kit::otel::…` alongside `http-client` / `sql`; `id-effect new --template service --db pg --otel` builds its `Database::sql()` capability with `instrumented_sql_client`. Wrap a client once: a second wrap opens a second span per call.

<!-- ignore: book_snippets builds id_effect_opentelemetry without its `platform` and `sql` features -->
```rust,ignore
use id_effect_opentelemetry::{TracedHttpClient, instrumented_sql_layer};
use id_effect_platform::http::ReqwestHttpClientProvider;
use id_effect_sql::sql_client_layer;

let http = TracedHttpClient::layer::<AppError>(ReqwestHttpClientProvider::layer());
let db = instrumented_sql_layer::<AppError>(sql_client_layer(pg_client), "postgresql");
let app = program.provide_layer(http.merge(db));
```

**Sampling.** `OtelConfig::sampler` is the head sampler of the OTLP tracer provider. `OtelConfig::from_env` reads the standard `OTEL_TRACES_SAMPLER` (`always_on`, `always_off`, `traceidratio`, `parentbased_always_on` (the default), `parentbased_always_off`, `parentbased_traceidratio`) and `OTEL_TRACES_SAMPLER_ARG` (the ratio, default `1.0`); `load_otel_config` also reads `otel.traces_sampler` / `otel.traces_sampler_arg`. Prefer a parent-based sampler in services that receive traced requests: it keeps every trace an upstream service decided to record and applies the ratio only to traces that start here.

```rust
use id_effect_opentelemetry::{OtelConfig, OtelSampler};

let config = OtelConfig::localhost("checkout")
    .with_sampler(OtelSampler::ParentBasedTraceIdRatio(0.1));
assert_eq!(config.sampler, OtelSampler::ParentBasedTraceIdRatio(0.1));
// For a provider built by hand: SdkTracerProvider::builder().with_sampler(config.sampler.sdk_sampler())
```

## See also

- Part II: [OpenTelemetry chapter](../part2/ch07-12-opentelemetry.md)
- Mission: `platform-observability`
