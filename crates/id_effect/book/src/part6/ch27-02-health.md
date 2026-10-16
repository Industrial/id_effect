# Health checks and readiness

A liveness probe asks "is the process up?"; a readiness probe asks "should this instance get traffic right now?". [`id_effect_axum::health`](https://docs.rs/id_effect_axum/latest/id_effect_axum/health/index.html) answers the second one with a **registry of effect-backed indicators**.

## Indicators are effects

A `HealthIndicator` is a name plus a factory for the `Effect` that probes one dependency. The effect is built afresh on every probe and reports a `HealthStatus` — `up`, `degraded` or `down` — with a JSON detail:

```rust
use std::convert::Infallible;
use std::time::Duration;

use id_effect::succeed;
use id_effect_axum::{HealthIndicator, HealthOutcome, HealthRegistry};
use serde_json::json;

# use id_effect_sql::health::sql_ping;
# use id_effect_sql::{TestSqlClient, sql_client_env};
# let env = sql_client_env(TestSqlClient::new());
# let router: axum::Router = axum::Router::new();
let registry = HealthRegistry::new()
  // Full control over the verdict.
  .with_indicator(HealthIndicator::new("queue", || {
    succeed::<_, Infallible, _>(HealthOutcome::degraded(json!({ "lag": 42 })))
  }))
  // Any effect: success is `up` with the value as detail, a failure is `down`.
  .with_indicator(
    HealthIndicator::from_effect("db", move || sql_ping().provide_all(env.clone()))
      .with_timeout(Duration::from_secs(2)),
  );

let app = router.merge(registry.routes()); // GET /health and GET /ready
```

- **Concurrent, bounded.** `HealthRegistry::check()` runs every indicator as a child fiber (`fork_all`, unbounded) and bounds each one with `Effect::timeout` (default 5 s). A check that fails, dies or times out is `down` with `{"error": ...}`; it never takes the others down with it.
- **Aggregation.** The report's status is the worst check: any `down` makes `/ready` answer **503**, `degraded` still answers **200**.
- **Response shape.**

```json
{
  "status": "down",
  "reason": "shutting_down",
  "checks": {
    "db":    { "status": "up", "detail": { "size": 4, "idle": 3, "max_size": 10 }, "duration_ms": 2 },
    "queue": { "status": "degraded", "detail": { "lag": 42 }, "duration_ms": 0 }
  }
}
```

`reason` is present only while the registry's `ReadinessState` is not ready.

## Built-in checks

| Check | Where | Detail |
|---|---|---|
| `sql_ping()` / `ping(&client)` | `id_effect_sql::health` | `SELECT 1` through any `SqlClient` (`null`) |
| `pg_pool_health(pool)` / `PgSqlClient::pool_health()` | `id_effect_sql::health` (feature `pg`) | `size`, `idle`, `max_size` |
| `HealthIndicator::cache(name, cache)` | `id_effect_axum` | `hits`, `misses`, `size` from `Cache::stats()` |
| `HealthIndicator::from_readiness(name, check)` | `id_effect_axum` | adapts a 0.7 `ReadinessCheck` predicate |

The SQL checks are plain effects — success is up, a `SqlError` is down — so `id_effect_sql` does not depend on the HTTP crate; `HealthIndicator::from_effect` turns them into indicators.

## Readiness flips first on shutdown

Gate the registry on the host's readiness:

```rust,no_run
# use id_effect_axum::{HealthIndicator, HealthRegistry, HostBuilder, HostError, serve_router};
# async fn run(db_indicator: HealthIndicator) -> Result<(), HostError> {
HostBuilder::new()
  .run_until_shutdown(|host| {
    let app = HealthRegistry::new()
      .with_indicator(db_indicator)
      .with_readiness(host.readiness.clone())
      .routes();
    serve_router(host, app)
  })
  .await?;
# Ok(())
# }
```

On SIGINT/SIGTERM, `run_until_shutdown` and `serve_router` call `ReadinessState::mark_shutting_down()` **before** draining: the listener keeps accepting while in-flight requests finish, so the load balancer sees `/ready` answer `503` with reason `shutting_down` and stops routing new traffic. `ServerHandle::shutdown` (and releasing `server_layer_with_readiness`) marks its readiness the same way before it stops accepting.

The plain-text helpers (`ready(check)`, `observability_routes_with_state`, `require_ready`) still work unchanged.

Run the example: `cargo run -p id_effect_axum --example 071_health`.

## See also

- [Observability and health](./ch27-00-observability.md)
- Integration test: `crates/id_effect_axum/tests/health.rs` (SIGTERM over a real socket)
