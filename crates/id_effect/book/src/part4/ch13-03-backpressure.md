# Backpressure Policies — Controlling Flow

Inside a stream pipeline there is no backpressure problem to solve: streams are pull-based, so a
source produces a chunk only when the consumer asks for one. The problem appears at the edge,
where a *push* producer (a socket, a callback, another thread) feeds a stream. If it produces
faster than the consumer pulls, something has to give: block the producer, drop data, or fail.
`id_effect` makes that choice explicit with `BackpressurePolicy`.

## stream_from_channel

`stream_from_channel(capacity)` returns a `Stream` and a `StreamSender` backed by a queue of at
most `capacity` chunks:

```rust,ignore
use id_effect::{Chunk, Effect, Exit, Never, StreamChannelFull, end_stream, send_chunk,
  stream_from_channel};

let (stream, sender) = stream_from_channel::<i32, &'static str, _>(3);
let send: Effect<(), StreamChannelFull> = send_chunk(&sender, Chunk::from_vec(vec![1, 2]));
assert!(send.block_on().into_result().is_ok());
let end: Effect<(), Never> = end_stream(sender);
assert_eq!(end.block_on(), Exit::succeed(()));
assert_eq!(stream.run_collect().block_on(), Exit::succeed(vec![1, 2]));
```

- `send_chunk(&sender, chunk)` is an effect, generic over `R`. Run it from a producer fiber, or
  `.block_on()` it on a producer thread.
- `end_stream(sender)` ends the stream after the buffered chunks; dropping the last sender ends
  it too.
- `sender.fail(e)` delivers `e` after every chunk already sent; it returns a `bool` and never
  waits (example `073_stream_producer_fail`).

## BackpressurePolicy

`stream_from_channel` uses `BoundedBlock`. `stream_from_channel_with_policy(capacity, policy)`
picks another one:

| Policy | When the queue is full |
|--------|------------------------|
| `BoundedBlock` | `send_chunk` suspends until there is room (nothing is lost) |
| `DropNewest` | the newly offered chunk is dropped |
| `DropOldest` | the oldest queued chunk is evicted to make room |
| `Fail` | `send_chunk` fails with `StreamChannelFull` |

```rust,ignore
use id_effect::{BackpressurePolicy, stream_from_channel_with_policy};

// Keep the freshest data; a slow consumer never stalls the producer.
let (stream, sender) =
  stream_from_channel_with_policy::<Reading, (), _>(1024, BackpressurePolicy::DropOldest);
```

`backpressure_decision(policy, queue_len, capacity)` is the pure decision function behind the
policies (example `075_backpressure_decision`).

## Choosing a Policy

| Scenario | Policy |
|----------|--------|
| Financial transactions: no data loss acceptable | `BoundedBlock` |
| Real-time sensor readings: only the latest matters | `DropOldest` |
| Log pipeline: shed excess load when overwhelmed | `DropNewest` |
| Overflow is a bug the producer must handle | `Fail` |

## Concurrency inside the pipeline

Effectful steps are sequential by default (`map_effect`). Bound concurrency explicitly with
`map_effect_par(n, f)`: at most `n` element effects run at once, as child fibers of the pulling
fiber, and the output keeps stream order. Since the next chunk is pulled only when the current one
is done, this is backpressure too.

```rust,ignore
let fetched = ids.map_effect_par(4, |id| fetch_record(id)); // at most 4 in flight
```

Other bounded buffers: `Sink::to_queue(queue)` suspends on a full bounded queue, and `broadcast`
gives each branch its own sliding queue.

## Summary

Choose the policy where a push producer meets a stream. `BoundedBlock` is the safe default but
can stall producers; `DropOldest` suits real-time data; `DropNewest` sheds load; `Fail` makes
overflow visible.
