# Streams: `Stream`, `Sink`, `stream_from_channel`, stream/fold channels

0.7 moves streaming to the fiber kernel as `id_effect::next::{Stream, Sink, …}`. A stream is now
an owned pull: each pull returns one chunk **and the rest of the stream**, so there is no shared
`Arc<Mutex<StreamState>>` locked on every pull, operators run under the consumer's fiber and
environment (no `R` clone), and a running stream is interrupted like any other effect. The 0.6
`id_effect::streaming` module stays until the v1 kernel is retired.

```rust
// 0.6
let out = pollster::block_on(
  Stream::from_iterable(0..10)
    .map(|n| n * 2)
    .filter(Box::new(|n: &i32| n % 3 == 0))
    .map_effect(|n| succeed::<i32, (), ()>(n + 1)) // Stream<i32, Or<(), ()>, ()>
    .run_collect()
    .run(&mut ()),
)?;

// 0.7
use id_effect::next::{Stream, succeed};
let out = Stream::<i32, MyErr>::from_iterable(0..10)
  .map(|n| n * 2)
  .filter(|n: &i32| n % 3 == 0)                  // any `Fn(&A) -> bool`, no `Box`
  .map_effect(|n| succeed(n + 1))                 // sequential, same `E`
  .run_collect()
  .block_on()
  .into_result()?;
```

- **Type parameters.** `Stream<A, E = Never, R = HNil>` (0.6: `E = (), R = ()`), like
  `next::Effect`. Constructors are generic over `E` and `R`; annotate one of them
  (`Stream::<u32, MyErr>::from_iterable(..)`) when nothing else fixes them.
- **`map_effect` is sequential** (ADR 0008 Amendment 2): one element's effect finishes before the
  next starts, so side effects happen in stream order. 0.6 ran every element of a chunk
  concurrently under the Fabric admission budget. For concurrency use
  **`map_effect_par(n, f)`**: at most `max(n, 1)` effects in flight (`Concurrency::Bounded(n)`
  per chunk, as child fibers), output in stream order. `map_effect`'s error is the stream's `E`
  (0.6: `Or<E, E2>`); map the effect's error with `.map_err(..)` inside `f`. `f` may be
  `FnMut`.
- **Pulling by hand.** `poll_next_chunk(&mut self, &mut env)` becomes `next_chunk(self)` (and
  `next_chunk_with_size(self, n)`), an effect yielding `Option<(Chunk<A>, Stream<A, E, R>)>`.
  A failed pull leaves no rest, so a stream is "fused" by construction. A size of `0` counts as
  `1` (0.6 returned `None` without consuming).
- **Sources.** `Stream::new(|env| fut)` is gone; use `from_effect(effect)` (single batch),
  `from_pull(|n| effect_of_option_vec)` (incremental; the pull returns an `Effect`, not a boxed
  future over `&mut R`), `unfold_effect`, `from_queue(queue)`, `Stream::from_channel(&channel)`
  or `channel.to_stream()`. New: `succeed`, `fail`, `from_vec`, `from_chunk`, `flat_map`,
  `concat`, `map_err`, `widen`, `run_drain`, `run(sink)`.
- **Cancellation.** The 0.6 consumers stopped early when `R` embedded a cancelled
  `CancellationToken`; 0.7 streams are interrupted through their fiber (`fork` +
  `interrupt`). Pulls that produce empty chunks yield to the executor, so they stay
  interruptible.
- **`stream_from_channel` / `StreamSender`.** `send_chunk(&sender, chunk)` is an effect generic
  over `R` that **suspends** while a `BoundedBlock` queue is full (0.6 spun `run_blocking` +
  `yield_now`); run it from a fiber, or `.block_on()` it on a producer thread.
  `end_stream(sender)` is `Effect<(), Never, R>`. `StreamSender::fail(e)` still returns `bool`
  but **never waits**: the error is delivered after every chunk already sent. Dropping the last
  sender now ends the stream (0.6 left the consumer waiting). `A` and `E` no longer need `Clone`.
- **Channels.** The stream-, sink- and fold-backed variants of the 0.6 `Channel` are separate
  types: `Channel::from_stream(s)` → `StreamChannel::new(s)` / `s.into_channel()`;
  `Channel::from_fold(init, Arc<step>)` → `FoldChannel::from_fold(init, step)`;
  `Channel::from_sink(sink)` → `FoldChannel::from_sink(sink)` (with `write`, `read`,
  `fold_state`, `consume_stream`, `map_out`, `flat_map_out`, `shutdown`). `channel.to_stream()`
  and `channel.to_sink()` work on the queue-backed `next::Channel`; read errors no longer carry
  `ChannelReadError`.
- **`Sink`.** `Sink<A, In, E = Never, R = HNil>`; `run(&self, stream)`; predicates are any
  `Fn(&In) -> bool`; new `Sink::new(driver)` and `Sink::for_each(f)`. `to_queue` suspends on a
  full bounded queue (0.6 dropped the item).
- **`broadcast`**: per-branch sliding queues instead of a `PubSub` + `Scope`; the pump yields
  after each element. `broadcast_with_replay(s, cap, replay, n)` keeps
  `max(cap, replay)` items per branch.
- **Throughput.** `with_throughput_metric(Metric)` becomes
  `with_throughput_counter(Arc<AtomicU64>)` until the next-kernel metrics land.
- `Chunk`, `Transducer` (and `transducer_map/filter/take`), the window, join, `state_scan` and
  `scan` operators keep their 0.6 behaviour.
