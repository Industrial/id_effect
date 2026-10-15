# `id_effect_opentelemetry`

**OpenTelemetry** traces, metrics, and logs for `id_effect` programs: `Effect::with_span`
spans and the `MetricRegistry` metrics of the 0.7 kernel are exported to OTEL.

See the mdBook chapter **"OpenTelemetry (`id_effect_opentelemetry`)"** and
[`docs/effect-ts-parity/phases/phase-b-opentelemetry.md`](../../docs/effect-ts-parity/phases/phase-b-opentelemetry.md).

## Features

| Feature | Purpose |
|---------|---------|
| `otlp` (default) | OTLP gRPC/HTTP exporters via [`install_from_config`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.install_from_config.html) |
| `config` | Load settings from [`id_effect_config`](https://docs.rs/id_effect_config) keys (`otel.*`) |
| `platform` | W3C inject/extract on [`id_effect_platform::http::HttpRequest`](https://docs.rs/id_effect_platform) |

Disable defaults for test-only builds:

```toml
id_effect_opentelemetry = { path = "../id_effect_opentelemetry", default-features = false }
```

## Production OTLP (one call)

```rust
use id_effect_opentelemetry::{OtelConfig, install_from_config, shutdown_otel_on_signal};

let guard = install_from_config(OtelConfig::from_env()?)?;
// `MetricRegistry::global()` is now exported; export spans by running the program under the hook:
let program = program.with_span_hook(guard.span_hook());
// ... run your server ...
shutdown_otel_on_signal(guard).await;
```

Environment variables follow the [OpenTelemetry spec](https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/):

- `OTEL_EXPORTER_OTLP_ENDPOINT` — collector URL (default `http://localhost:4317`)
- `OTEL_EXPORTER_OTLP_PROTOCOL` — `grpc` or `http`
- `OTEL_SERVICE_NAME` — `service.name` resource attribute
- `OTEL_EXPORTER_OTLP_HEADERS` — comma-separated `key=value` pairs
- `RUST_LOG` — optional `EnvFilter` when installing the global subscriber

## Highlights

- **Span bridge**: `OtelSpanHook` is a `SpanHook` installed with `Effect::with_span_hook` (a
  `FiberRef`, inherited by forked fibers): every `with_span` becomes an OTEL span, parented
  across fibers on any executor thread; `current_otel_context` reads the active context back.
- **W3C propagation**: trace context + baggage on portable header maps (and `HttpRequest` with `platform`).
- **Metrics bridge**: `bridge_metrics` / `bridge_global_metrics` export a `MetricRegistry`
  (counters and gauges pulled from the registry at collection, histograms, summaries and
  frequencies pushed by a registry listener).
- **Logs bridge**: `tracing` events exported to OTEL logs via `opentelemetry-appender-tracing`.
- **Unified starter**: [`install_otel_starter`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.install_otel_starter.html) wires traces, metrics, logs, and propagators.
- **Test harness**: [`with_otel_test_harness`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.with_otel_test_harness.html) — in-memory exporters, no global clashes.

## Examples

```bash
cargo run -p id_effect_opentelemetry --example otel_minimal
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 cargo run -p id_effect_opentelemetry --example otel_production
```

## Tests

```bash
cargo test -p id_effect_opentelemetry --all-features
```
