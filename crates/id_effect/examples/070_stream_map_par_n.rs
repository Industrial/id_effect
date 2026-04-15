//! Ex 070 — `map_par_n` applies a pure map with bounded parallelism.
use id_effect::{Stream, run_blocking, succeed};

fn main() {
  let s = Stream::from_iterable([1_i32, 2, 3, 4]).map_par_n(2, |n| succeed::<i32, (), ()>(n * 10));
  let mut v = run_blocking(s.run_collect(), ()).expect("collect");
  v.sort();
  assert_eq!(v, vec![10, 20, 30, 40]);
  println!("070_stream_map_par_n ok");
}
