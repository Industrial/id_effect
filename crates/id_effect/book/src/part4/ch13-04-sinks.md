# Sinks — Consuming Streams

A `Stream` describes a sequence of values. A consumer drives it and turns it into one `Effect`.
Together they form a pipeline: `Stream` → operators → consumer.

## Built-in Consumers

Most of the time you use a consuming method on `Stream`. Each turns a `Stream<A, E, R>` into an
`Effect<B, E, R>` that you run with `.block_on()` at the edge or compose further:

```rust
use id_effect::{Effect, Stream};
# use id_effect::{caps, succeed};
# #[derive(Debug)]
# struct DbError;
# struct Db;
# struct Order { amount: u64 }
# fn orders() -> Stream<Order, DbError, caps!(Db)> { Stream::from_iterable([Order { amount: 5 }, Order { amount: 7 }]) }
# fn events() -> Stream<String, DbError, caps!(Db)> { Stream::from_iterable(["login".to_owned()]) }
# fn log_event(e: String) -> Effect<(), DbError, caps!(Db)> { succeed(()).map(move |()| println!("{e}")) }

let all: Effect<Vec<u32>, ()> = Stream::<u32, ()>::from_iterable([1, 2, 3]).run_collect();
let total: Effect<u64, DbError, caps!(Db)> = orders().run_fold(0, |acc, o| acc + o.amount);
let logged: Effect<(), DbError, caps!(Db)> = events().run_for_each(|e| log_event(e));
let biggest: Effect<Option<u64>, ()> = Stream::<u64, ()>::from_iterable([3, 9, 4])
  .run_reduce(|a, b| a.max(b));
let drained: Effect<(), DbError, caps!(Db)> = events().run_drain();
# assert!(matches!(total.provide(Db).block_on(), id_effect::Exit::Success(12)));
# assert!(matches!(biggest.block_on(), id_effect::Exit::Success(Some(9))));
# assert!(logged.provide(Db).block_on().into_result().is_ok());
# assert!(drained.provide(Db).block_on().into_result().is_ok());
# assert!(matches!(all.block_on(), id_effect::Exit::Success(v) if v == [1, 2, 3]));
```

`run_fold_effect(init, |acc, a| effect)` folds with an effect per element.

## Sink values

A `Sink<A, In, E = Never, R = HNil>` is a reusable consumer of a `Stream<In, E, R>` producing `A`.
Run it with `sink.run(stream)` or `stream.run(sink)`:

```rust
use id_effect::{Sink, Stream};

let sum = Sink::<u32, u32>::fold_left(0, |a, x| a + x);
let total = Stream::from_iterable([1_u32, 2, 3]).run(sum); // Effect<u32, Never>
# assert_eq!(total.block_on(), id_effect::Exit::succeed(6));
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

```rust
# use std::path::PathBuf;
# use id_effect::{Effect, Exit, Sink, Stream, caps, succeed};
# #[derive(Debug)]
# struct IoError;
# struct Files;
# struct Record(u32);
# fn append_lines(_path: PathBuf, batch: Vec<Record>) -> Effect<usize, IoError, caps!(Files)> { succeed(batch.len()) }
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
# let records = Stream::<Record, IoError, caps!(Files)>::from_iterable((0..300).map(Record));
# let written = write_csv(PathBuf::from("out.csv")).run(records).provide(Files).block_on();
# assert!(matches!(written, Exit::Success(300)));
```

Batch with `grouped(n)` to write several records per system call. If the stream fails, the effect
fails with that error; wrap the sink's resource in `acquire_use_release` or a scoped `Layer` when
cleanup must always run.

## Bounding Infinite Streams

A consumer doesn't know whether its stream is finite. Bound an infinite stream with `take(n)` or
`take_while(pred)` before running it, or run it on a fiber and interrupt that fiber (a stream is
interrupted like any other effect):

```rust
# use id_effect::{Effect, Exit, Stream, succeed};
# fn events() -> Stream<u64, String> { Stream::unfold(0, |n| Some((n, n + 1))) }
# fn process(_event: u64) -> Effect<(), String> { succeed(()) }
let first_hundred = events().take(100).run_collect();
let until_shutdown = events().run_for_each(process).fork(); // later: handle.interrupt()
# assert!(matches!(first_hundred.block_on(), Exit::Success(v) if v.len() == 100));
# let stopped = until_shutdown.flat_map(|handle| handle.interrupt());
# assert!(matches!(stopped.block_on(), Exit::Success(Exit::Failure(_))));
```

## Summary

| Consumer | Returns | Use when |
|----------|---------|----------|
| `run_collect()` / `Sink::collect()` | `Vec<A>` | small result sets |
| `run_fold(init, f)` / `Sink::fold_left` | `B` | one aggregated value |
| `run_for_each(f)` / `Sink::for_each` | `()` | an effect per element |
| `run_drain()` / `Sink::drain()` | `()` | side effects only |
| `run(sink)` | sink's `A` | reusable or custom consumers |
