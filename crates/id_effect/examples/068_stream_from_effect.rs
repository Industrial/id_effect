//! Ex 068 — `Stream::from_effect` lifts a one-shot effect into a stream.
use id_effect::{Stream, run_blocking, succeed};

fn main() {
  let s = Stream::from_effect(succeed::<Vec<i32>, (), ()>(vec![1, 2, 3]));
  assert_eq!(run_blocking(s.run_collect(), ()), Ok(vec![1, 2, 3]));
  println!("068_stream_from_effect ok");
}
