# Backpressure Policies — Controlling Flow

A stream is a pipeline: a producer emits data, operators transform it, a consumer processes it. Problems arise when the producer is faster than the consumer. Backpressure is the mechanism that handles this mismatch.

## The Problem

```
Producer: emits 10,000 events/sec
Consumer: processes 1,000 events/sec

What happens to the 9,000 surplus events per second?
```

Your options are: block the producer, drop events, or buffer them. Each is correct in different contexts. id_effect makes the choice explicit via `BackpressurePolicy`.

## BackpressurePolicy

```rust
use id_effect::BackpressurePolicy;

// Block the producer until the consumer catches up (default for bounded channels)
BackpressurePolicy::Block

// Drop the newest events when the buffer is full
BackpressurePolicy::DropLatest

// Drop the oldest events when the buffer is full (keep the freshest data)
BackpressurePolicy::DropOldest

// Unbounded buffering — never drop, never block (use carefully)
BackpressurePolicy::Unbounded
```

## Applying a Policy to a Channel-Backed Stream

The most common place to specify backpressure is when bridging from a channel to a `Stream`:

```rust
use id_effect::{stream_from_channel_with_policy, BackpressurePolicy};
use std::sync::mpsc;

let (tx, rx) = mpsc::channel::<Event>();

// Drop old events; always reflect the latest state
let stream = stream_from_channel_with_policy(rx, 1024, BackpressurePolicy::DropOldest);
```

Contrast with `stream_from_channel`, which uses `Block` by default. If you don't think about backpressure at this point, `Block` is the safe choice — you won't lose data, but a slow consumer will slow down the producer.

## Choosing a Policy

| Scenario | Policy |
|----------|--------|
| Financial transactions — no data loss acceptable | `Block` |
| Real-time sensor readings — only latest matters | `DropOldest` |
| Log pipeline — drop excess if overwhelmed | `DropLatest` |
| Batch import — control memory, halt on overflow | `Block` |
| Dashboard metrics — fresh data over completeness | `DropOldest` |

## Stream-Level Backpressure

Streams composed with `flat_map` or `merge` also have implicit backpressure: downstream operators signal upstream when they can accept more work. This happens automatically and doesn't require a policy setting — the `Stream` runtime handles it.

For explicit control over concurrency in `flat_map`:

```rust
stream
    .flat_map_with_concurrency(4, |id| fetch_record(id))
    // Only 4 fetch_record effects run concurrently
    // Others wait until a slot frees — natural backpressure
```

## Monitoring Drops

When using `DropLatest` or `DropOldest`, you often want to know how many events were dropped:

```rust
let (stream, dropped_counter) = stream_from_channel_with_policy_and_counter(
    rx,
    1024,
    BackpressurePolicy::DropOldest,
);

// Periodically log the counter
effect! {
    loop {
        let n = dropped_counter.load(Ordering::Relaxed);
        if n > 0 {
            ~ log.warn(format!("Dropped {n} events due to backpressure"));
        }
        ~ sleep(Duration::from_secs(10));
    }
}
```

## Summary

Always choose a backpressure policy explicitly. The default (`Block`) is safe but can stall producers. `DropOldest` is often right for real-time data. `DropLatest` is right when order matters but throughput doesn't. `Unbounded` is only acceptable when the rate is truly bounded by the domain.
