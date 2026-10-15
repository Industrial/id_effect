# Metrics, `MetricRegistry` and span hooks (`id_effect::next`)

0.7 replaces the v1 `Metric<In, Out>` (state behind `Arc<Mutex<u64>>` / `Mutex<Vec<Duration>>`,
unbounded sample vectors, no registry) with Effect.ts-style metrics on lock-free state, a keyed
`MetricRegistry`, fiber metric tags and span hooks.

```rust
// 0.6
let m = Metric::counter("requests", vec![("route".into(), "/a".into())]);
run_blocking(m.apply(1), ())?;
let h = Metric::histogram("latency", Vec::new());       // unbounded Vec<Duration>
let eff = h.track_duration(handler());

// 0.7
let requests = Counter::new("requests").tagged("route", "/a");   // global registry
let eff = requests.increment().then(handler());                  // or requests.track_all(1, handler())
let latency = Histogram::new("latency", Boundaries::exponential(0.001, 2.0, 12));
let eff = latency.track_duration(handler()).tag_metrics("service", "api");
let pairs = MetricRegistry::global().snapshot();                 // keys with tags + states
```

- Kinds: `Counter` (`u64`, saturating), `Gauge` (`i64`, `set` / `adjust`), `Histogram` (fixed
  `Boundaries`, `boundaries + 1` buckets, the last `+inf`), `Summary` (`SummaryConfig::new(max_age,
  max_size, quantiles)`: a bounded window), `Frequency` (counts per string). State is atomics; the
  summary window is the only lock.
- A metric is a `MetricKey` (name, `MetricKind`, sorted tags) in a registry; the same key yields the
  same series. `tagged(k, v)` is a different series. `MetricRegistry::new()` isolates (tests).
- Updates are effects through the `Metric` trait (`update`, `track_all`, `track`,
  `track_success_with`, `track_error_with`, `track_exit_with`, `track_duration_with`) or
  `update_now` outside an effect. Durations are recorded in seconds on the fiber's executor clock
  (virtual on `TestExecutor`). v1 `make(..)` / `apply(..)` have no `next` counterpart.
- `Effect::tag_metrics(k, v)` (a `FiberRef`, inherited by forked children) tags every metric update
  inside.
- Exporters: `MetricRegistry::snapshot()` (pull) and `MetricListener` via `add_listener` (push).
- Spans: `Effect::with_span(name)` / `with_span_attrs`, `Span::current()`; an exporter implements
  `SpanHook` and installs it with `Effect::with_span_hook` (a `FiberRef`, no thread-locals).
