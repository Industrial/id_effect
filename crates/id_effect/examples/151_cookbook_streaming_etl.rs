//! Ex 151 — Cookbook: streaming ETL with bounded memory.
//!
//! Extract from an unbounded source, transform with `map_serial` / `filter` / `tumbling`, and load
//! with a fold. Streams are pull-based: every stage asks upstream for one chunk at a time, so the
//! pipeline holds at most a chunk plus one partial window, however large the source is. The
//! `assert_bounded_memory` helper proves that by panicking if the pipeline buffers its upstream.
//!
//! Run: `cargo run -p id_effect --example 151_cookbook_streaming_etl`
//! Test: `cargo test -p id_effect --example 151_cookbook_streaming_etl`

use id_effect::testing::assert_bounded_memory;
use id_effect::{Stream, run_blocking};

/// One parsed input record.
#[derive(Clone, Debug, PartialEq)]
struct Reading {
  seq: u64,
  value: u64,
}

/// Summary of one tumbling window.
#[derive(Clone, Debug, PartialEq)]
struct WindowStat {
  first_seq: u64,
  count: usize,
  max: u64,
}

/// Extract: an endless feed of raw `"seq,value"` lines (think: a log tail or a paginated API).
fn extract() -> Stream<String, (), ()> {
  Stream::unfold(0_u64, |seq| {
    Some((format!("{seq},{}", (seq * 37) % 101), seq + 1))
  })
}

/// Parse a raw line; malformed lines become `None` and are dropped downstream.
fn parse(line: String) -> Option<Reading> {
  let (seq, value) = line.split_once(',')?;
  Some(Reading {
    seq: seq.parse().ok()?,
    value: value.parse().ok()?,
  })
}

/// Transform: drop outliers, then summarise every 10 surviving readings.
fn transform(readings: Stream<Reading, (), ()>) -> Stream<WindowStat, (), ()> {
  readings
    .filter(Box::new(|r: &Reading| r.value < 100))
    .tumbling(10)
    .map(|window: Vec<Reading>| WindowStat {
      first_seq: window[0].seq,
      count: window.len(),
      max: window.iter().map(|r| r.value).max().unwrap_or(0),
    })
}

fn main() {
  // Load: fold 1 000 windows into a total. `take` stops pulling from the infinite source.
  let pipeline = transform(
    extract()
      .map_serial(parse)
      .filter(Box::new(|r: &Option<Reading>| r.is_some()))
      .map_serial(|r| r.expect("filtered")),
  )
  .take(1_000);
  let (windows, readings) = run_blocking(
    pipeline.run_fold((0_usize, 0_usize), |(w, n), stat| (w + 1, n + stat.count)),
    (),
  )
  .expect("pipeline");
  println!("loaded {windows} windows covering {readings} readings");
  assert_eq!((windows, readings), (1_000, 10_000));

  // The same transform, run against the instrumented source of `assert_bounded_memory`:
  // it panics if more than 256 source items are ever produced but not yet delivered.
  let first = assert_bounded_memory(
    |s| {
      transform(s.map(|seq| Reading {
        seq,
        value: (seq * 37) % 101,
      }))
    },
    50,
    256,
  );
  assert_eq!(first.len(), 50);
  assert_eq!(first[0].first_seq, 0);
  assert!(first.iter().all(|w| w.max < 100));
  println!("first window {:?}", first[0]);
  println!("151_cookbook_streaming_etl ok");
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn transform_keeps_memory_bounded_on_an_infinite_source() {
    let stats = assert_bounded_memory(
      |s| {
        transform(s.map(|seq| Reading {
          seq,
          value: seq % 120,
        }))
      },
      100,
      256,
    );
    assert_eq!(stats.len(), 100);
    assert!(stats.iter().all(|w| w.count == 10 && w.max < 100));
  }

  #[test]
  fn parse_drops_malformed_lines() {
    assert_eq!(
      parse("7,42".to_string()),
      Some(Reading { seq: 7, value: 42 })
    );
    assert_eq!(parse("garbage".to_string()), None);
  }
}
