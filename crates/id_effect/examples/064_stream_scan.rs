//! Ex 064 — `scan` carries state while emitting mapped outputs.
use id_effect::Exit;
use id_effect::next::Stream;

fn main() {
  let s = Stream::<i64, ()>::range(1, 5).scan(0_i64, |acc, x| {
    *acc += x;
    *acc
  });
  assert_eq!(s.run_collect().block_on(), Exit::succeed(vec![1, 3, 6, 10]));
  println!("064_stream_scan ok");
}
