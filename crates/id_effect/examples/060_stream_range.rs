//! Ex 060 — `Stream::range` emits `start..end`, then ends.
use id_effect::Exit;
use id_effect::Stream;

fn main() {
  let s = Stream::<i64, ()>::range(1, 5).run_collect();
  assert_eq!(s.block_on(), Exit::succeed(vec![1, 2, 3, 4]));
  println!("060_stream_range ok");
}
