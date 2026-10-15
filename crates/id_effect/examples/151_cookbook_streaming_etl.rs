//! Ex 151 — Cookbook: pull-based streaming ETL with bounded memory.
//!
//! An infinite `extract` source of CSV-ish lines is parsed, filtered and grouped into tumbling
//! windows of 10 readings. The stream is pull-based: `take(n)` on the windowed stream stops the
//! source after the readings it needs, so memory stays bounded however long the source is.
//!
//! Run: `cargo run -p id_effect --example 151_cookbook_streaming_etl`
//! (tests: `cargo test -p id_effect --example 151_cookbook_streaming_etl`).

use id_effect::next::Stream;

#[derive(Clone, Debug, PartialEq)]
struct Reading {
  seq: u64,
  value: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct WindowStat {
  first_seq: u64,
  count: usize,
  max: u64,
}

fn extract() -> Stream<String, ()> {
  Stream::unfold(0_u64, |seq| {
    Some((format!("{seq},{}", (seq * 37) % 101), seq + 1))
  })
}

fn parse(line: String) -> Option<Reading> {
  let (seq, value) = line.split_once(',')?;
  Some(Reading {
    seq: seq.parse().ok()?,
    value: value.parse().ok()?,
  })
}

fn transform(readings: Stream<Reading, ()>) -> Stream<WindowStat, ()> {
  readings
    .filter(|r: &Reading| r.value < 100)
    .tumbling(10)
    .map(|window: Vec<Reading>| WindowStat {
      first_seq: window[0].seq,
      count: window.len(),
      max: window.iter().map(|r| r.value).max().unwrap_or(0),
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

fn main() {
  let (windows, readings) = pipeline()
    .take(1_000)
    .run_fold((0_usize, 0_usize), |(w, n), stat| (w + 1, n + stat.count))
    .block_on()
    .into_result()
    .expect("pipeline");
  println!("loaded {windows} windows covering {readings} readings");
  assert_eq!((windows, readings), (1_000, 10_000));

  let first = pipeline()
    .take(50)
    .run_collect()
    .block_on()
    .into_result()
    .expect("first windows");
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
  fn transform_stops_an_infinite_source_after_the_windows_it_needs() {
    let source = Stream::<u64, ()>::unfold(0_u64, |seq| Some((seq, seq + 1)));
    let stats = transform(source.map(|seq| Reading {
      seq,
      value: seq % 120,
    }))
    .take(100)
    .run_collect()
    .block_on()
    .into_result()
    .expect("windows");
    assert_eq!(stats.len(), 100);
    assert!(stats.iter().all(|w| w.count == 10 && w.max < 100));
  }
}
