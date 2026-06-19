# Observability and health

Part VI observability centers on [`id_effect_opentelemetry`](../../../id_effect_opentelemetry) and Axum health routes in [`id_effect_axum`](../../../id_effect_axum).

## Starter layer

Call [`install_otel_starter`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.install_otel_starter.html) once at process startup to register traces, metrics, logs, and W3C propagation:

```rust
use id_effect_opentelemetry::{install_otel_starter, OtelStarterConfig};

let _guard = install_otel_starter(OtelStarterConfig::from_env())?;
```

For tests, use `OtelStarterConfig::in_memory_for_tests()`.

## Health and readiness

Mount [`id_effect_axum::health::observability_routes`](https://docs.rs/id_effect_axum/latest/id_effect_axum/health/fn.observability_routes.html) on your router:

```rust
use id_effect_axum::health::{observability_routes_with_state, ReadinessState};
use std::sync::Arc;

let ready = ReadinessState::new(false);
let obs = observability_routes_with_state(ready.clone());
// flip to true after DB pool warm-up
ready.set_ready(true);
```

## Axum trace middleware

[`id_effect_opentelemetry::trace_request`](https://docs.rs/id_effect_opentelemetry/latest/id_effect_opentelemetry/fn.trace_request.html) extracts W3C `traceparent` and creates a server span:

```rust
use axum::middleware;
use id_effect_opentelemetry::trace_request;

let app = router.layer(middleware::from_fn(trace_request));
```

Pair with [`id_effect_rpc::span`](../../../id_effect_rpc/src/span.rs) helpers for RPC-shaped routes.

## See also

- Part II: [OpenTelemetry chapter](../part2/ch07-12-opentelemetry.md)
- Mission: `platform-observability`
