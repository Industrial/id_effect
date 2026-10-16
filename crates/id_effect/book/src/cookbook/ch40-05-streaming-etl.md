# Streaming ETL with bounded memory

**Problem.** You need to extract records from a source that is large or never ends (a log tail, a
paginated API, a message queue), transform them, and load an aggregate. Memory must stay flat no
matter how much data passes through. You also want a test that fails if someone later adds a stage
that buffers everything.

**Recipe.** Build the pipeline from `Stream` operators. Streams are pull-based: each stage asks its
upstream for at most one chunk at a time, `take(n)` stops pulling once it has `n` items, and
windows hold at most one partial window.

```rust
use id_effect::Stream;

#[derive(Clone, Debug, PartialEq)]
struct Reading { seq: u64, value: u64 }

#[derive(Clone, Debug, PartialEq)]
struct WindowStat { first_seq: u64, count: usize, max: u64 }

/// Extract: an endless feed of raw "seq,value" lines.
fn extract() -> Stream<String, ()> {
  Stream::unfold(0_u64, |seq| Some((format!("{seq},{}", (seq * 37) % 101), seq + 1)))
}

fn parse(line: String) -> Option<Reading> {
  let (seq, value) = line.split_once(',')?;
  Some(Reading { seq: seq.parse().ok()?, value: value.parse().ok()? })
}

/// Transform: drop outliers, summarise every 10 surviving readings.
fn transform(readings: Stream<Reading, ()>) -> Stream<WindowStat, ()> {
  readings
    .filter(|r: &Reading| r.value < 100)
    .tumbling(10)
    .map(|w: Vec<Reading>| WindowStat {
      first_seq: w[0].seq,
      count: w.len(),
      max: w.iter().map(|r| r.value).max().unwrap_or(0),
    })
}

fn pipeline() -> Stream<WindowStat, ()> {
  transform(
    extract()
      .map_serial(parse)
      .filter(|r: &Option<Reading>| r.is_some())
      .map_serial(|r| r.expect("filtered")),
  )
}

// Load: fold 1 000 windows. `take` is what makes the infinite source finite.
let (windows, readings) = pipeline()
  .take(1_000)
  .run_fold((0_usize, 0_usize), |(w, n), stat| (w + 1, n + stat.count))
  .block_on()
  .into_result()
  .expect("pipeline");
assert_eq!((windows, readings), (1_000, 10_000));
```

## Proving the memory bound in a test

Run the transform over an *infinite* source and take a bounded number of outputs. A pull-based
pipeline finishes; a stage that drains its whole input first (for example
`Stream::from_effect(s.run_collect())`) never returns, so the test hangs instead of passing. Run
it under your test runner's timeout (nextest's slow-test timeout, say) to turn that into a failure:

```rust
# use id_effect::Stream;
# #[derive(Clone, Debug, PartialEq)]
# struct Reading { seq: u64, value: u64 }
# #[derive(Clone, Debug, PartialEq)]
# struct WindowStat { first_seq: u64, count: usize, max: u64 }
# fn transform(readings: Stream<Reading, ()>) -> Stream<WindowStat, ()> {
#   readings
#     .filter(|r: &Reading| r.value < 100)
#     .tumbling(10)
#     .map(|w: Vec<Reading>| WindowStat {
#       first_seq: w[0].seq,
#       count: w.len(),
#       max: w.iter().map(|r| r.value).max().unwrap_or(0),
#     })
# }
// In a #[cfg(test)] module, under #[test]:
fn transform_stops_an_infinite_source_after_the_windows_it_needs() {
  let source = Stream::<u64, ()>::unfold(0_u64, |seq| Some((seq, seq + 1)));
  let stats = transform(source.map(|seq| Reading { seq, value: seq % 120 }))
    .take(100)
    .run_collect()
    .block_on()
    .into_result()
    .expect("windows");
  assert_eq!(stats.len(), 100);
  assert!(stats.iter().all(|w| w.count == 10 && w.max < 100));
}
# transform_stops_an_infinite_source_after_the_windows_it_needs();
```

To bound the number of in-flight items exactly, wrap the source in `Stream::from_pull` with a
counter of items produced and assert, per pull, that it never runs more than one chunk
(`DEFAULT_CHUNK_SIZE`, 64) plus a filter's skips ahead of what the consumer has received.

Full example, including the test: [`crates/id_effect/examples/151_cookbook_streaming_etl.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/examples/151_cookbook_streaming_etl.rs)

```bash
cargo run -p id_effect --example 151_cookbook_streaming_etl
cargo test -p id_effect --example 151_cookbook_streaming_etl
```

## Notes

- A stream ends at its first error: a failed pull returns no rest. To skip bad records, parse into
  `Option` or `Result` *values* as above, rather than failing the stream.
- `map` and `filter` may run a large chunk on Rayon through the Compute Fabric. Use `map_serial` /
  `filter_serial` for stateful (`FnMut`) or order-sensitive steps.
- For a custom incremental source (a paginated API), use `Stream::from_pull(|max| effect)` or
  `Stream::unfold_effect`; for a push source, `stream_from_channel` with a
  [backpressure policy](../part4/ch13-03-backpressure.md).

See also: [Streams](../part4/ch13-00-streams.md), [Windowing](../part5/ch22-01-windowing.md),
[Backpressure policies](../part4/ch13-03-backpressure.md).
