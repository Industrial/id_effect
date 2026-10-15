//! Ex 069 — `map_effect` runs an effect per element; a failure ends the stream with it.
use id_effect::{Cause, Exit};
use id_effect::{Stream, fail, succeed};

fn main() {
  let s = Stream::<i32, &'static str>::from_iterable([1, 2, 3]).map_effect(|n| succeed(n * 2));
  assert_eq!(s.run_collect().block_on(), Exit::succeed(vec![2, 4, 6]));

  let bad = Stream::<i32, &'static str>::from_iterable([1]).map_effect(|_| fail::<i32, _, _>("e"));
  assert_eq!(
    bad.run_collect().block_on(),
    Exit::Failure(Cause::Fail("e"))
  );
  println!("069_stream_map_effect ok");
}
