# `observability` — Stratum 15: metrics & tracing

**Instrumentation** for effects and fibers: [`Metric`](metric.rs) (counters, gauges, histograms via `metric_make`), and [`tracing`](tracing.rs) integration — [`TracingConfig`](tracing.rs), [`with_span`](tracing.rs), [`emit_effect_event`](tracing.rs) / [`emit_fiber_event`](tracing.rs), [`install_tracing_layer`](tracing.rs), [`snapshot_tracing`](tracing.rs), [`TracingFiberRefs`](tracing.rs).

## What lives here

| Module | Role |
|--------|------|
| `metric` | `Metric`, `metric_make` — structured metrics keyed in hash maps. |
| `tracing` | Spans, fiber/effect events, snapshotting, fiber-ref bridges for trace context. |

## What it is used for

- **SLIs** for schedulers, pools, queues (rates, queue depth, retries).
- **Correlating** logs/traces with `FiberId` and effect boundaries.
- **Test assertions** on span trees via snapshot helpers.

## Best practices

1. **Install** tracing layers once at startup (`install_tracing_layer`); avoid per-request layer stacks.
2. **Keep span names stable** — they become dashboards and alerts.
3. **Cardinality** — avoid high-cardinality labels on `Metric` keys (user IDs as unbounded labels burn memory).
4. **Pair** with [`concurrency::FiberRef`](../concurrency/README.md) when you need per-fiber trace baggage.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 15.
- [`runtime`](../runtime/README.md) — where execution starts; hooks often wrap `run_*`.
- [`scheduling`](../scheduling/README.md) — clock + metrics for retry storms.
