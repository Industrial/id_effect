# Metrics and Prometheus

Every metric you record with `Counter`, `Gauge`, `Histogram`, `Summary` or `Frequency` lives in a
`MetricRegistry` (`MetricRegistry::global()` unless you build your own). This chapter turns that
registry into a Prometheus scrape endpoint, adds runtime and HTTP metrics with no handler code,
and instruments the SQL client. No `prometheus` crate is involved: the text format is written by
`id_effect::observability::prometheus`.

## The scrape endpoint

Merge `metrics_router()` into your app and wrap the whole router in `http_metrics_layer()`:

```rust,ignore
use axum::Router;
use id_effect_axum::metrics::{http_metrics_layer, metrics_router};
use id_effect_axum::routing;

let app = Router::new()
    .route("/users/{id}", routing::get(get_user))
    .merge(metrics_router())          // GET /metrics
    .layer(http_metrics_layer())      // every request, matched or not
    .with_state(state);
```

`GET /metrics` answers `200` with `Content-Type: text/plain; version=0.0.4; charset=utf-8` and
the exposition text:

```text
# TYPE http_server_requests_total counter
http_server_requests_total{method="GET",route="/users/{id}",status="200"} 2
http_server_requests_total{method="GET",route="unmatched",status="404"} 1
# TYPE id_effect_executor_in_flight gauge
id_effect_executor_in_flight 0
# TYPE id_effect_fibers_live gauge
id_effect_fibers_live 0
# TYPE id_effect_fibers_started_total counter
id_effect_fibers_started_total 3
```

`metrics_route_for(registry)` serves another registry; `metrics_route_with(registry, on_scrape)`
runs `on_scrape` first, which is where gauges that must be sampled (a pool's connections) are set.
To render without Axum, call `prometheus::render(&registry)` (or `render_pairs` with extra
`MetricPair`s) and send `prometheus::CONTENT_TYPE`.

## How each kind is exposed

| Kind | Exposed as |
|---|---|
| `Counter` | `counter`, named `<name>_total` (the suffix is added only if missing) |
| `Gauge` | `gauge` |
| `Histogram` | `histogram`: cumulative `_bucket{le=..}` lines ending with `le="+Inf"`, then `_sum` and `_count` |
| `Summary` | `summary`: one line per configured quantile (`quantile=".."`, `NaN` on an empty window), then `_sum` and `_count` |
| `Frequency` | `counter` `<name>_total`, one series per observed value (`value=".."`) |

Tags become labels. Label values are escaped (`\\`, `\"`, `\n`), and characters that Prometheus
does not allow in names become `_`.

## Runtime metrics

The kernel counts fibers with two relaxed atomics, read when the endpoint renders:

- `id_effect_fibers_live`: fibers that have started and are not done yet. A fiber stops counting
  exactly once, when it is marked done (or, for a context that never ran to completion, when it is
  dropped).
- `id_effect_fibers_started_total`: every fiber started since the process began.
- `id_effect_executor_in_flight`: request fibers running on the global `EffectExecutor`.

Starting a fiber costs two uncontended `fetch_add`s and finishing it one `fetch_sub`. Nothing is
recorded into the registry on the fork path. Read the values directly with
`runtime_metrics::fibers_live()` / `fibers_started()`.

## HTTP metrics

`http_metrics_layer()` records two series per request:

- `http_server_requests_total{method,route,status}`
- `http_server_request_duration_seconds{method,route}` (buckets from 5 ms to 10 s)

`route` is the route template from axum's `MatchedPath` (`/users/{id}`), never the raw path.
Requests that match no route are all recorded under `route="unmatched"`, so a scanner probing
random URLs cannot create new series. Non-standard methods are recorded as `_OTHER`. Apply the
layer with `Router::layer`. `route_layer` only sees matched routes.

## Database metrics

Wrap any `SqlClient` in `InstrumentedSqlClient` before providing it:

```rust,ignore
use id_effect_sql::metrics::InstrumentedSqlClient;
use id_effect_sql::sql_client_layer;

let layer = sql_client_layer::<AppError>(InstrumentedSqlClient::new(pg_client));
```

Each `connect`, `query`, `execute` and `begin` records:

- `db_client_operations_total{op,outcome}`, where `outcome` is `ok` or `error` (a typed failure,
  defect or interrupt)
- `db_client_operation_duration_seconds{op}`

SQL text is never used as a label. With the `pg` feature, `metrics::record_pg_pool(&client,
&registry)` sets `db_client_connection_count{state="idle"|"used"}` and
`db_client_connection_max` from the pool. Call it from the scrape hook:

```rust,ignore
let registry = MetricRegistry::global().clone();
let route = metrics_route_with(registry.clone(), move || record_pg_pool(&client, &registry));
```

## Run it

`cargo run -p id_effect_axum --example 070_metrics` builds a router with the layer and the
endpoint, sends a few requests, and prints the scrape.

## See also

- [Observability and health](./ch27-00-observability.md): OpenTelemetry export of the same
  registry.
