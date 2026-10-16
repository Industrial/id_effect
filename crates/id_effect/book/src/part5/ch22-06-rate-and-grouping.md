# Rate limiting and grouping

[`time.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/time.rs) adds operators that act on **time**, and [`group.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/group.rs) splits a stream **per key**. Runnable together in `examples/076_stream_zip_time_group.rs`.

## The fiber clock

Time-based operators read the clock of the pulling fiber's executor and sleep on its timer — the same clock `sleep`, `timeout` and schedules use. Under the `TokioExecutor` that is real time; under a [`TestExecutor`](../part4/ch15-02-test-clock.md) it is virtual time that moves only with `advance`, so tests are instant and deterministic. A chunk is treated as arriving at one instant.

## Throttle

`throttle(units, per)` delays elements so that at most `units` pass per `per`, with bursts of up to `units` (Effect.ts `throttle` with the `"shape"` strategy). Nothing is dropped: a pull that is over the rate sleeps until the next element conforms, and upstream is only pulled once the buffered chunk is emitted, so a slow rate backpressures the source.

```rust
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Stream, TestExecutor};

let exec = TestExecutor::new();
let h = Stream::<u32>::from_iterable(1..=3)
    .throttle(1, Duration::from_secs(1))
    .run_collect()
    .fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
assert!(!h.is_done(), "the second element waits one virtual second");
exec.advance(Duration::from_secs(2));
assert!(h.is_done());
```

`throttle_enforce(units, per)` is the `"enforce"` strategy: elements above the rate are **dropped** and the operator never sleeps.

## Debounce

`debounce(d)` emits an element only once `d` has passed without a newer one; the pending element is flushed when the source ends. Upstream is driven by a child fiber into a one-chunk buffer, so it keeps being pulled while the timer runs and is backpressured when the consumer is slower.

```rust
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Stream, TestExecutor, sleep, succeed};

// One element per pull, after a delay: 1@0ms, 2@100ms, 3@200ms, 4@1000ms.
let mut plan: VecDeque<(u64, u32)> = VecDeque::from([(0, 1), (100, 2), (100, 3), (800, 4)]);
let source = Stream::<u32>::from_pull(move |_| match plan.pop_front() {
    None => succeed(None),
    Some((ms, x)) => sleep(Duration::from_millis(ms)).map(move |()| Some(vec![x])),
});
let exec = TestExecutor::new();
let h = source
    .debounce(Duration::from_millis(300))
    .run_collect()
    .fork_root(Arc::new(exec.clone()));
exec.advance(Duration::from_secs(2));
assert!(h.is_done()); // emitted [3, 4]: 3 settled at 500 ms, 4 flushed at the end
```

## Group by key

`group_by_key(buffer, key)` yields `(key, substream)` once per distinct key, in order of first appearance. A router fiber pulls upstream and offers each element to its group's queue of `buffer` elements, **waiting while it is full**: that is the backpressure, and it means the substreams must be consumed concurrently (fork each, or use `group_by`). Dropping a substream discards its key's later elements instead of blocking the others.

`group_by(buffer, key, f)` runs `f(key, substream)` for every group concurrently and merges the outputs (Effect.ts `groupBy(…).evaluate`):

```rust
use id_effect::{Exit, Stream};

let sums = Stream::<u32>::from_iterable(1..=10).group_by(
    4,
    |n| n % 2,
    |parity, group| {
        Stream::from_effect(group.run_fold(0, |acc, n| acc + n).map(move |s| vec![(parity, s)]))
    },
);
let mut out = match sums.run_collect().block_on() {
    Exit::Success(v) => v,
    Exit::Failure(c) => panic!("{c:?}"),
};
out.sort();
assert_eq!(out, vec![(0, 30), (1, 25)]);
```

Memory is bounded: `buffer` elements per open group, plus `buffer` announced groups and `buffer` output chunks. An upstream failure fails the outer stream (`group_by_key`) or the result (`group_by`), as does a failure of any group's stream.

## Stopping early

Every operator here releases upstream when the consumer stops early (`take`, a failure downstream, interruption). `zip` and `throttle` drop upstream with the rest of the stream; `debounce` and `group_by` interrupt their background fibers when the stream is dropped, so an in-flight upstream pull is interrupted and its finalizers run — the fibers are also children of the pulling fiber, so they never outlive it.
