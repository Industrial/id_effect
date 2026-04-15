# Sinks — Consuming Streams

A `Stream` describes a sequence of values. A `Sink` describes how to consume them. Together they form a complete pipeline: `Stream` → operators → `Sink`.

## Built-in Sinks

Most of the time you don't write a `Sink` explicitly — you use one of the consuming methods on `Stream`:

```rust
// Collect all elements into a Vec
let users: Effect<Vec<User>, DbError, Db> = all_users().collect();

// Fold into a single value
let total: Effect<u64, DbError, Db> = orders()
    .fold(0u64, |acc, order| acc + order.amount);

// Run a side-effecting action for each element
let logged: Effect<(), DbError, Db> = events()
    .for_each(|event| log_event(event));

// Drain (discard all values, run for side effects only)
let drained: Effect<(), DbError, Db> = events()
    .map(|e| emit_metric(e))
    .drain();

// Take the first N elements
let first_ten: Effect<Vec<User>, DbError, Db> = all_users()
    .take(10)
    .collect();
```

Each of these methods turns a `Stream<A, E, R>` into an `Effect<B, E, R>`, which you can then run with `run_blocking` or compose further.

## The Sink Trait

When the built-in consumers aren't enough, implement `Sink`:

```rust
use id_effect::{Sink, Chunk, Effect};

struct CsvWriter {
    path: PathBuf,
    written: usize,
}

impl Sink<Record> for CsvWriter {
    type Error = IoError;
    type Env   = ();

    fn on_chunk(
        &mut self,
        chunk: Chunk<Record>,
    ) -> Effect<(), IoError, ()> {
        effect! {
            for record in &chunk {
                ~ self.write_csv_line(record);
            }
            ()
        }
    }

    fn on_done(&mut self) -> Effect<(), IoError, ()> {
        effect! {
            ~ self.flush();
            ()
        }
    }
}
```

`on_chunk` is called for each chunk of elements. `on_done` is called once when the stream ends — use it to flush buffers or close handles.

## Running a Stream into a Sink

```rust
let writer = CsvWriter::new("output.csv");

let effect: Effect<(), IoError, Db> = all_records()
    .run_into_sink(writer);

run_blocking(effect, env)?;
```

`run_into_sink` drives the stream and feeds each chunk to the sink. If the stream fails, `on_done` is not called — use resource scopes around the sink when cleanup is unconditionally required.

## Sink Composition

Sinks can be composed: a `ZipSink` feeds the same stream to two sinks simultaneously:

```rust
let count_sink  = CountSink::new();
let csv_sink    = CsvWriter::new("out.csv");

// Both sinks receive every element
let combined = ZipSink::new(count_sink, csv_sink);

all_records().run_into_sink(combined)
```

Each element is delivered to both sinks in order. If either sink fails, the whole pipeline fails.

## Finite vs Infinite Streams and Sinks

A `Sink` doesn't know whether its stream is finite or infinite. Combine with `take`, `take_while`, or `take_until` to bound an infinite stream before running it into a sink:

```rust
// Process at most 1 hour of events
let one_hour = Duration::from_secs(3600);

event_stream()
    .take_until(sleep(one_hour))
    .for_each(|event| process(event))
```

## Summary

| Method | Returns | Use when |
|--------|---------|----------|
| `.collect()` | `Effect<Vec<A>, …>` | Small result sets that fit in memory |
| `.fold(init, f)` | `Effect<B, …>` | Single aggregated value |
| `.for_each(f)` | `Effect<(), …>` | Side effects per element |
| `.drain()` | `Effect<(), …>` | Discard results, keep side effects |
| `.run_into_sink(s)` | `Effect<(), …>` | Custom consumption logic |
