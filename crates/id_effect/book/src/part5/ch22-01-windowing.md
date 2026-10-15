# Windowing

[`window.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/window.rs) adds count- and time-based grouping on [`Stream`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/stream.rs). Every window is a `Vec<A>`.

## Count windows

```rust
use id_effect::{Exit, Stream};

let sums = Stream::<i32>::from_iterable(1..=10)
    .tumbling(3) // non-overlapping windows: [1,2,3] [4,5,6] [7,8,9] [10]
    .map(|w| w.iter().sum::<i32>())
    .run_collect()
    .block_on();
assert_eq!(sums, Exit::succeed(vec![6, 15, 24, 10]));
```

- **`tumbling(n)`** — alias of `grouped`; the last window may be short.
- **`sliding(size, step)`** — overlapping windows (`A: Clone`); `size == 0` or `step == 0` yields an empty stream.

## Time and session windows

Pass a timestamp extractor `FnMut(&A) -> Instant`:

```rust
use std::time::{Duration, Instant};
use id_effect::Stream;

fn sessions(events: Vec<(Instant, char)>) -> Stream<Vec<(Instant, char)>> {
    Stream::from_iterable(events).session_by_gap(Duration::from_secs(30), |(ts, _)| *ts)
}
```

- **`session_by_gap(gap, ts)`** — a session ends when the next element is more than `gap` later; it is emitted as soon as the first element of the next session arrives (the last one when the stream ends).
- **`tumbling_by_time(duration, ts)`** — fixed-width buckets on a shared epoch.
- **`sliding_by_time(duration, step, ts)`** — overlapping time ranges stepped by `step`; elements older than the next window start are pruned, so memory is bounded by one `duration` of elements.

Timestamps come from the elements, not from a clock, so time windows are deterministic in tests.
