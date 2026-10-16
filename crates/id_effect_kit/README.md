# `id_effect_kit`

One front door to [`id_effect`](../id_effect): a single dependency with a feature flag per battery, whole-crate re-exports and a `prelude`.

```toml
[dependencies]
id_effect_kit = { version = "0.8", features = ["web", "otel"] }
# The derive and `effect!` macros expand to `::id_effect::` paths; keep the core crate too.
id_effect = "0.8"
```

| Feature | Adds | Re-exported as |
|---|---|---|
| always on | `id_effect`, `id_effect_cli` (`run_main`), `id_effect_config` (`Secret`) | `id_effect_kit::{id_effect, cli, config}` |
| `web` | `id_effect_axum` | `id_effect_kit::axum` |
| `sql-pg` | `id_effect_sql` with the `pg` driver | `id_effect_kit::sql` |
| `otel` | `id_effect_opentelemetry` (OTLP, config loading) | `id_effect_kit::otel` |
| `cluster` | `id_effect_node` (with `web`: also `axum::cluster`) | `id_effect_kit::node` |
| `full` | all of the above | |

## Quick start

```rust
use std::process::ExitCode;

use id_effect_kit::prelude::*;

#[derive(Debug)]
enum AppError {
  Unconfigured,
}

fn token() -> Effect<Secret<String>, AppError> {
  succeed(Secret::new("s3cr3t".to_string()))
}

fn program() -> Effect<usize, AppError> {
  token().flat_map(|t| {
    if t.expose().is_empty() {
      fail(AppError::Unconfigured)
    } else {
      succeed(t.expose().len())
    }
  })
}

fn main() -> ExitCode {
  // Secrets never print: `Debug` shows `<redacted>`.
  assert_eq!(format!("{:?}", Secret::new("x")), "<redacted>");
  run_main(program(), hlist![], RunMainConfig::minimal())
}
```

The per-feature prelude items: `web` adds `EffectExecutor`, `execute`, `routing`, `serve_until_shutdown`, `SecurityLayer`, `health_router`, `readiness_router`, `observability_routes` and `metrics_router`; `sql-pg` adds `SqlClientService`, `SqlError` and `with_transaction`; `otel` adds `OtelConfig` and `install_from_config`.

## Tier

`integration` (ADR 0010 Amendment 4). It moves to `platform` (ADR 0012) once a reference app under `examples/reference/` depends on it.

## Examples

`cargo run -p id_effect_kit --example 010_prelude` (add `--features full` for the battery section).
