# `observability` — log levels, metrics, spans and the tracing buffer

| Module | Items |
|--------|-------|
| `log_level` | `LogLevel` (shared with `id_effect_config` and `id_effect_logger`). |
| `metric` | Effect.ts `Metric` semantics: `Counter`, `Gauge`, `Histogram` (`Boundaries`), `Summary`, `Frequency`, the `Metric` trait (`update`, `track*`), `MetricKey` / `MetricState`. |
| `registry` | `MetricRegistry` (`global()` or isolated `new()`), tagged snapshots, `MetricListener`. |
| `mod.rs` | `Span`, `SpanHook`, `Effect::with_span` / `with_span_attrs` / `with_span_hook`, `tag_metrics`: the hooks `id_effect_opentelemetry` builds on. |
| `tracing` | The in-process buffer: `install_tracing_layer`, `snapshot_tracing`, `tracing_hook`, `annotate_current_span`, `emit_*` / `record_*` events. |

```rust
use id_effect::{Effect, Exit, Metric, MetricRegistry, succeed};

let registry = MetricRegistry::new();
let requests = registry.counter("requests");
let eff: Effect<u32, ()> = requests.track_all(1, succeed(7)).with_span("handler");
assert_eq!(eff.block_on(), Exit::succeed(7));
```

Spans are recorded only under a span hook: wrap the program with
`.with_span_hook(tracing_hook())` for the in-process buffer, or use the OpenTelemetry layer of
`id_effect_opentelemetry`. All per-fiber state (current span, metric tags) lives in `FiberRef`
slots, never in thread-locals.

Keep span names stable and metric tags low-cardinality (no unbounded user ids as tags).

The 0.6 `metric_make`, the free `with_span` and `TracingFiberRefs` were removed in 0.7.
