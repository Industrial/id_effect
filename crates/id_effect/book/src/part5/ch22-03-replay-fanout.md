# Replay fanout

[`broadcast_with_replay`](../../src/streaming/replay.rs) extends [`Stream::broadcast`](../../src/streaming/stream.rs) with a sliding replay tail.

```rust
use id_effect::{Stream, broadcast_with_replay, run_async};

let src = Stream::from_iterable(1..=100);
let (mut branches, pump) = run_async(
    broadcast_with_replay(src, /* hub */ 64, /* replay tail */ 16, /* branches */ 2),
    (),
)
.await?;

// Run `pump` concurrently with pulls on each branch (same pattern as `broadcast`).
```

- **`hub_capacity`** — sliding [`PubSub`](../../src/coordination/pubsub.rs) ring size.
- **`replay_len`** — number of recent items retained in the shared replay buffer (also seeded into each branch buffer as the pump runs).
- **`Stream::broadcast_replay(cap, branches)`** — convenience wrapper using `replay_len == cap`.

Return shape matches broadcast: `(Vec<Stream<…>>, pump_effect)`.
