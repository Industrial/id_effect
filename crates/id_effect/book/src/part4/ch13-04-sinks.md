# Sinks — Consuming Streams

A `Stream` describes a sequence of values. A consumer drives it and turns it into one `Effect`.
Together they form a pipeline: `Stream` → operators → consumer.

## Built-in Consumers

Most of the time you use a consuming method on `Stream`. Each turns a `Stream<A, E, R>` into an
`Effect<B, E, R>` that you run with `.block_on()` at the edge or compose further:

```rust,ignore
use id_effect::{Effect, Stream};

let all: Effect<Vec<u32>, ()> = Stream::<u32, ()>::from_iterable([1, 2, 3]).run_collect();
let total: Effect<u64, DbError, caps!(Db)> = orders().run_fold(0, |acc, o| acc + o.amount);
let logged: Effect<(), DbError, caps!(Db)> = events().run_for_each(|e| log_event(e));
let biggest: Effect<Option<u64>, ()> = Stream::<u64, ()>::from_iterable([3, 9, 4])
  .run_reduce(|a, b| a.max(b));
let drained: Effect<(), DbError, caps!(Db)> = events().run_drain();
```

`run_fold_effect(init, |acc, a| effect)` folds with an effect per element.

## Sink values

A `Sink<A, In, E = Never, R = HNil>` is a reusable consumer of a `Stream<In, E, R>` producing `A`.
Run it with `sink.run(stream)` or `stream.run(sink)`:

```rust,ignore
use id_effect::{Sink, Stream};

let sum = Sink::<u32, u32>::fold_left(0, |a, x| a + x);
let total = Stream::from_iterable([1_u32, 2, 3]).run(sum); // Effect<u32, Never>
```

| Constructor | Produces |
|-------------|----------|
| `Sink::collect()` | `Vec<In>` |
| `Sink::fold_left(init, f)` / `Sink::from_fold(init, f)` | the fold result |
| `Sink::collect_all_while(pred)` / `collect_all_until(pred)` | the leading elements |
| `Sink::drain()` | `()` |
| `Sink::for_each(\|x\| effect)` | `()`, running an effect per element |
| `Sink::to_queue(queue)` | `()`, offering every element to a `Queue` (suspends when full) |
| `Sink::collect_to_map()` | `HashMap<K, V>` from `(K, V)` elements |
| `Sink::new(\|stream\| effect)` | anything: a custom driver over the whole stream |

`fold_sink.zip(other_fold_sink)` feeds every element to two fold sinks at once and yields both
results as a pair (both sides must be `fold_left` sinks).

## A Custom Sink

`Sink::new` takes a driver from the stream to an effect, so a custom sink composes the stream
consumers you already know:

```rust,ignore
fn write_csv(path: PathBuf) -> Sink<usize, Record, IoError, caps!(Files)> {
  Sink::new(move |records: Stream<Record, IoError, caps!(Files)>| {
    let path = path.clone();
    records
      .grouped(256)
      .run_fold_effect(0, move |written, batch| {
        append_lines(path.clone(), batch).map(move |n| written + n)
      })
  })
}
```

Batch with `grouped(n)` to write several records per system call. If the stream fails, the effect
fails with that error; wrap the sink's resource in `acquire_use_release` or a scoped `Layer` when
cleanup must always run.

## Bounding Infinite Streams

A consumer doesn't know whether its stream is finite. Bound an infinite stream with `take(n)` or
`take_while(pred)` before running it, or run it on a fiber and interrupt that fiber (a stream is
interrupted like any other effect):

```rust,ignore
let first_hundred = events().take(100).run_collect();
let until_shutdown = events().run_for_each(process).fork(); // later: handle.interrupt()
```

## Summary

| Consumer | Returns | Use when |
|----------|---------|----------|
| `run_collect()` / `Sink::collect()` | `Vec<A>` | small result sets |
| `run_fold(init, f)` / `Sink::fold_left` | `B` | one aggregated value |
| `run_for_each(f)` / `Sink::for_each` | `()` | an effect per element |
| `run_drain()` / `Sink::drain()` | `()` | side effects only |
| `run(sink)` | sink's `A` | reusable or custom consumers |
