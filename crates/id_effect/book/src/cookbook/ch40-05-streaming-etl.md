# Streaming ETL with bounded memory

**Problem.** You need to extract records from a source that is large or never ends (a log tail, a
paginated API, a message queue), transform them, and load an aggregate. Memory must stay flat no
matter how much data passes through. You also want a test that fails if someone later adds a stage
that buffers everything.

**Recipe.** Build the pipeline from `Stream` operators. Streams are pull-based: each stage asks its
upstream for at most one chunk at a time, `take(n)` stops pulling once it has `n` items, and
windows hold at most one partial window. Guard the transform with
`id_effect::testing::assert_bounded_memory`.

```rust
use id_effect::{Stream, run_blocking};

#[derive(Clone, Debug)]
struct Reading { seq: u64, value: u64 }

#[derive(Clone, Debug)]
struct WindowStat { first_seq: u64, count: usize, max: u64 }

/// Extract: an endless feed of raw "seq,value" lines.
fn extract() -> Stream<String, (), ()> {
  Stream::unfold(0_u64, |seq| Some((format!("{seq},{}", (seq * 37) % 101), seq + 1)))
}

fn parse(line: String) -> Option<Reading> {
  let (seq, value) = line.split_once(',')?;
  Some(Reading { seq: seq.parse().ok()?, value: value.parse().ok()? })
}

/// Transform: drop outliers, summarise every 10 surviving readings.
fn transform(readings: Stream<Reading, (), ()>) -> Stream<WindowStat, (), ()> {
  readings
    .filter(Box::new(|r: &Reading| r.value < 100))
    .tumbling(10)
    .map(|w: Vec<Reading>| WindowStat {
      first_seq: w[0].seq,
      count: w.len(),
      max: w.iter().map(|r| r.value).max().unwrap_or(0),
    })
}

// Load: fold 1 000 windows. `take` is what makes the infinite source finite.
let pipeline = transform(
  extract()
    .map_serial(parse)
    .filter(Box::new(|r: &Option<Reading>| r.is_some()))
    .map_serial(|r| r.expect("filtered")),
)
.take(1_000);
let (windows, readings) = run_blocking(
  pipeline.run_fold((0, 0), |(w, n), stat| (w + 1, n + stat.count)),
  (),
)
.expect("pipeline");
assert_eq!((windows, readings), (1_000, 10_000));
```

## Proving the memory bound in a test

`assert_bounded_memory(pipeline, n, max_in_flight)` runs your pipeline over an instrumented
infinite source of `u64`s and takes `n` outputs. It panics as soon as more than `max_in_flight`
source items have been produced but not yet delivered downstream. An operator that drains its
upstream first therefore fails the test quickly instead of hanging. Pick `max_in_flight` of at least
one chunk (64 items) plus whatever a selective `filter` may skip between outputs.

```rust
#[test]
fn transform_keeps_memory_bounded_on_an_infinite_source() {
  let stats = assert_bounded_memory(
    |s| transform(s.map(|seq| Reading { seq, value: seq % 120 })),
    100, // outputs to take
    256, // max source items in flight
  );
  assert_eq!(stats.len(), 100);
}
```

If someone adds a stage that collects its whole input first, such as
`Stream::from_effect(s.run_collect())`, this test panics with "the pipeline is buffering its
upstream".

Full example, including the test: [`crates/id_effect/examples/151_cookbook_streaming_etl.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/examples/151_cookbook_streaming_etl.rs)

```bash
cargo run -p id_effect --example 151_cookbook_streaming_etl
cargo test -p id_effect --example 151_cookbook_streaming_etl
```

## Notes

- A stream ends at its first error: the next poll returns `Ok(None)`. To skip bad records, parse
  into `Option` or `Result` *values* as above, rather than failing the stream.
- `map` and `filter` may use the Compute Fabric's parallelism within one chunk. Use `map_serial` /
  `filter_serial` for stateful (`FnMut`) steps.
- `Stream::from_iterable` needs an owned iterator (`I::IntoIter: 'static`). For a custom
  incremental source, use `Stream::from_pull`.

See also: [Streams](../part4/ch13-00-streams.md), [Windowing](../part5/ch22-01-windowing.md),
[Backpressure policies](../part4/ch13-03-backpressure.md).
