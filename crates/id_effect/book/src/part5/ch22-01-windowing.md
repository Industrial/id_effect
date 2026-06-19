# Windowing

[`window`](../../src/streaming/window.rs) adds count- and time-based grouping on [`Stream`](../../src/streaming/stream.rs).

## Count windows

```rust
use id_effect::Stream;

let sums = Stream::from_iterable(1..=10)
    .tumbling(3)   // non-overlapping chunks → Vec<_>
    .map(|chunk| chunk.iter().sum::<i32>());
```

- **`tumbling(n)`** — alias of [`grouped`](../../src/streaming/stream.rs); last chunk may be short.
- **`sliding(size, step)`** — overlapping windows; `size == 0` or `step == 0` yields an empty stream.

## Time and session windows

Provide a timestamp extractor `Fn(&A) -> Instant`:

```rust
use id_effect::Stream;
use std::time::{Duration, Instant};

let events: Vec<(Instant, char)> = /* ... */;
let sessions = Stream::from_iterable(events)
    .session_by_gap(Duration::from_secs(30), |(ts, _)| *ts);
```

- **`tumbling_by_time(duration, ts)`** — fixed-width buckets aligned to `Instant::UNIX_EPOCH`.
- **`sliding_by_time(duration, step, ts)`** — overlapping time ranges stepped by `step`.

Time buckets use [`merge_time_bucket`](../../src/streaming/stream.rs) helpers internally for ordered aggregation maps.
