# The operator console

Metrics and traces leave the process; the console looks inside it. [`id_effect_console`](https://docs.rs/id_effect_console) serves a dashboard and a JSON API that show the live fibers, the supervision trees, the metric registry, the health checks and the log level, change the log level without a restart, and (feature `jobs`) report on a job queue. Everything it serves sits behind an [`id_effect_authz`](./ch30-02-authorization.md) guard.

## Mounting it

`Console` collects what the console shows; `Console::router(guard)` turns it into an Axum router. There is no unguarded constructor: the guard is in front of the dashboard page at `/_console`, of every API route under `/_console/api/` and of the API's fallback, so a principal the guard denies gets a Problem Details `401` (anonymous) or `403` and learns nothing about the routes.

```rust
use std::convert::Infallible;

use id_effect::succeed;
use id_effect_authz::{Guard, require_role};
use id_effect_axum::{HealthIndicator, HealthOutcome, HealthRegistry};
use id_effect_console::{Console, ConsoleConfig};

let health = HealthRegistry::new().with_indicator(HealthIndicator::new("db", || {
  succeed::<_, Infallible, _>(HealthOutcome::up())
}));

let console: axum::Router = Console::new()
  .health(health)
  // .supervisor("app", app_supervisor_handle)
  // .env_filter(otel_guard.env_filter().clone())
  .router(Guard::new("console.operate", require_role("operator")));

// Merge it into the app *inside* the authentication layer that sets the principal:
let app = axum::Router::new().merge(console);
// let app = app.layer(session_or_bearer_authentication);

// A console on a non-loopback address must restrict the guard to operators.
assert!(ConsoleConfig::new("0.0.0.0:9090".parse().unwrap()).requires_guard());
# let _ = app;
```

The builder takes the supervisors to show (`supervisor(id, handle)`, nested ones too), the `HealthRegistry` to run, the `MetricRegistry` to read (default: the global one), the reload handle of the subscriber's `EnvFilter` and the timeout of each `which_children` call. `ConsoleConfig` says where a dedicated console listener binds: on loopback a lenient guard such as `authenticated()` is acceptable, anywhere else `requires_guard()` is true and the guard must name a role or permission.

## The dashboard

`GET /_console` answers one static page compiled into the crate with `include_str!`: plain HTML, inline CSS and vanilla JavaScript, with no build step and nothing loaded from another origin. It polls the API every two seconds (1 s to 10 s, or paused) and shows

| Panel | Reads | Shows |
|---|---|---|
| Health | `GET health` | the overall status and every check with its status, duration and detail |
| Log level | `GET` / `PUT log-level` | the `EnvFilter` directive and the `EffectLogger` minimum, and a form that changes them |
| Supervision trees | `GET supervisors` | each root supervisor's children, nested supervisors expanded, stopped children marked; unreachable supervisors with their error |
| Jobs | `GET jobs` | per-queue counts by state and the newest 50 jobs (hidden when `jobs` answers `404`) |
| Fibers | `GET fibers` | every live fiber: id, parent, state, age, children and span stack |
| Metrics | `GET metrics` | every series with its kind, tags and reading, filterable by name |

The page calls the API relative to its own path with same-origin credentials, so it works wherever the console router is merged and with whatever cookie or session authentication the app uses. It renders every value as text (`textContent`), never as markup, and its `Content-Security-Policy` (`default-src 'none'; connect-src 'self'; frame-ancestors 'none'`, inline script and style only) stops it loading or sending anything elsewhere and from being framed. Like every console answer it carries `Cache-Control: no-store`.

## The API

Every route answers JSON (`application/json`), and every error is Problem Details (`application/problem+json`):

| Route | Answers |
|---|---|
| `GET /_console/api/fibers` | `[FiberView]` from the core `fiber_dump()` (`fiber-registry` feature), by id |
| `GET /_console/api/supervisors` | `SupervisorsView { roots, unreachable }` from `which_children` |
| `GET /_console/api/metrics` | `[MetricView]`: the registry, then `id_effect_fibers_live` and `id_effect_fibers_started_total` |
| `GET /_console/api/health` | the `HealthRegistry` report, `200` even when down (the console shows; `/ready` probes) |
| `GET /_console/api/log-level` | `LogLevelView { filter, logger }` |
| `PUT /_console/api/log-level` | `LogLevelChange { level?, filter? }`, validated before anything changes |
| `GET /_console/api/jobs` | `JobsView { queues, recent }` (feature `jobs`) |

A `level` (`trace` … `fatal`, `none`) sets the process-wide `EffectLogger` minimum and, unless a `filter` directive is given too, the `EnvFilter` to the same level; a `filter` needs the reload handle (`Console::env_filter`). A bad level or directive is a `400` and changes nothing. The form's `PUT` carries `Content-Type: application/json`, so a cross-site page cannot send it without a CORS preflight; do not put a permissive CORS layer in front of the console.

## The jobs panel

With the `jobs` feature, `Console::jobs(queue)` takes any `id_effect_jobs::JobQueue` (`PgJobQueue`, `MemoryJobQueue`). `GET jobs` then answers each queue's counts by state with its oldest runnable `run_at`, and the newest 50 jobs with their state, attempts, `run_at` and last error. Payloads are left out: they may carry personal data, and the console is for operating the queue, not reading its data. A failing queue is a `500` Problem; without a registered queue `jobs` is a `404` and the dashboard hides the panel.

```rust
use id_effect::WallClock;
use id_effect_authz::{Guard, require_role};
use id_effect_console::Console;
use id_effect_jobs::MemoryJobQueue;

// A `PgJobQueue` in production; the console only reads it (`stats`, `list`).
let queue = MemoryJobQueue::new(WallClock::system());
let console: axum::Router = Console::new()
  .jobs(queue.clone())
  .router(Guard::new("console.operate", require_role("operator")));
# let _ = console;
```

## Run it

`cargo run -p id_effect_console --example 020_console` serves the console on a loopback port behind a bearer-token authentication layer, fetches the page and every API route without and with the test credential, changes the log level and exits. Add `-- --serve` to keep it running on `127.0.0.1:9090` and open `/_console` in a browser (that mode treats every request as the local operator, which is only acceptable on loopback).

## See also

- [Observability and health](./ch27-00-observability.md), [Metrics](./ch27-01-metrics.md), [Health checks and readiness](./ch27-02-health.md)
- [Authorization](./ch30-02-authorization.md) for guards and policies
- Integration test over a real socket: `crates/id_effect_console/tests/console.rs`
