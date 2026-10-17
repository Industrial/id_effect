# `id_effect_console`

The operator console for id_effect services: live fibers from the core `fiber-registry` feature,
supervision trees, metrics, health and the log level, behind an `id_effect_authz` guard (ADR 0010
Amendment 6).

`Console::router(guard)` is the only way to get the routes: JSON under `/_console/api/`, every
route and the console's fallback behind the guard (`401` anonymous, `403` otherwise).

| Route | Answers |
|---|---|
| `GET fibers` | every live fiber (core `fiber_dump`) |
| `GET supervisors` | the registered supervisors' trees, from `which_children` |
| `GET metrics` | the metric registry and the runtime fiber gauges |
| `GET health` | the `HealthRegistry` report |
| `GET` / `PUT log-level` | the reloadable `EnvFilter` and the `EffectLogger` minimum, changed without a restart |
| `GET jobs` | per-queue counts and the newest jobs of an `id_effect_jobs` queue (feature `jobs`) |

The same router serves the dashboard at `GET /_console`, behind the same guard: one static page
(inline CSS and vanilla JavaScript, no build step, nothing loaded from another origin) that polls
the API and shows health, the log level with a form to change it, the supervision trees, the job
queues, the live fibers and the metrics.

```rust
use id_effect_authz::{Guard, require_role};
use id_effect_console::Console;

let console: axum::Router = Console::new()
  // .supervisor("app", supervisor_handle)
  // .env_filter(otel_guard.env_filter().clone())
  .router(Guard::new("console", require_role("operator")));
```

`ConsoleConfig` is where the console listens. A console bound to a loopback address serves the
local machine only and may use a lenient guard; on any other address the guard must restrict it
to operators:

```rust
use id_effect_console::ConsoleConfig;

let local = ConsoleConfig::loopback(9090);
assert_eq!(local.bind().to_string(), "127.0.0.1:9090");
assert!(!local.requires_guard());

let public = ConsoleConfig::new("0.0.0.0:9090".parse().unwrap());
assert!(public.requires_guard());
```

Tier `integration`.
