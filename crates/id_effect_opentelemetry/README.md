# `id_effect_opentelemetry`

Phase B integration: **OpenTelemetry** traces, metrics, and logs alongside `id_effect`'s built-in
[`with_span`](https://docs.rs/id_effect/latest/id_effect/fn.with_span.html) and
[`Metric`](https://docs.rs/id_effect/latest/id_effect/struct.Metric.html) helpers.

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

- **`with_span_otel`**: composes `id_effect::with_span` with a `tracing` span exported via OTEL.
- **W3C propagation**: trace context + baggage on portable header maps (and `HttpRequest` with `platform`).
- **Metric bridges**: dual-write from `Metric` counters/histograms to OTEL instruments.
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
