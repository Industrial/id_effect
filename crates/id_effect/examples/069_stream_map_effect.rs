//! Ex 069 — `map_effect` runs an effect per element (`Or` widens errors).
use id_effect::{Or, Stream, fail, run_blocking, succeed};

fn main() {
  let s = Stream::from_iterable([1_i32, 2, 3]).map_effect(|n| succeed::<i32, (), ()>(n * 2));
  assert_eq!(run_blocking(s.run_collect(), ()), Ok(vec![2, 4, 6]));

  let bad = Stream::from_iterable([1_i32]).map_effect(|_| {
    succeed::<i32, &'static str, ()>(0).flat_map(|_| fail::<i32, &'static str, ()>("e"))
  });
  let r = run_blocking(bad.run_collect(), ());
  assert!(matches!(r, Err(Or::Right("e"))));
  println!("069_stream_map_effect ok");
}
