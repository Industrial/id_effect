//! Ex 064 — `scan` carries state while emitting mapped outputs.
use id_effect::{Stream, run_blocking};

fn main() {
  let s = Stream::range(1, 5).scan(0_i64, |acc, x| {
    *acc += x;
    *acc
  });
  assert_eq!(run_blocking(s.run_collect(), ()), Ok(vec![1, 3, 6, 10]));
  println!("064_stream_scan ok");
}
