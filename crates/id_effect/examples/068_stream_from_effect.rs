//! Ex 068 — `Stream::from_effect` lifts a one-shot effect yielding a `Vec` into a stream.
use id_effect::Exit;
use id_effect::next::{Stream, succeed};

fn main() {
  let s = Stream::<i32, ()>::from_effect(succeed(vec![1, 2, 3]));
  assert_eq!(s.run_collect().block_on(), Exit::succeed(vec![1, 2, 3]));
  println!("068_stream_from_effect ok");
}
