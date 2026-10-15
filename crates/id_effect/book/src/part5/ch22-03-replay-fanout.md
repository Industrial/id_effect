# Replay fanout

[`broadcast_with_replay`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/broadcast.rs) extends `Stream::broadcast` with a replay tail: each branch first receives the most recent `replay_len` items, then the live feed.

Both return an effect yielding `(branches, pump)`: a `Vec` of branch streams and a pump effect that drives the source into the hub. Run the pump and the branch consumers together:

```rust
use id_effect::{Effect, Stream, broadcast_with_replay};

let src = Stream::<u32>::from_iterable(1..=100);
let program: Effect<(Vec<u32>, Vec<u32>)> =
    broadcast_with_replay(src, /* hub */ 64, /* replay tail */ 16, /* branches */ 2)
        .flat_map(|(mut branches, pump)| {
            let (b, a) = (branches.pop().unwrap(), branches.pop().unwrap());
            pump.then(a.run_collect().zip(b.run_collect()))
        });
```

- **`hub_capacity`** — size of the sliding [`PubSub`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/coordination/pubsub.rs) ring; a slow branch drops the oldest items, it never blocks the pump.
- **`replay_len`** — number of recent items retained and replayed to each branch.
- **`Stream::broadcast_replay(cap, branches)`** — convenience wrapper with `replay_len == cap`.

The shape is the same as `Stream::broadcast(capacity, branches)`.
