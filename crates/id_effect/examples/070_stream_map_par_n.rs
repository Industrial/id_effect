//! Ex 070 — `Stream::map_effect` with Fabric admission-bounded concurrency.

use id_effect::{Stream, runtime::run_blocking, succeed};

fn main() {
  let s = Stream::from_iterable([1_i32, 2, 3, 4]).map_effect(|n| succeed::<i32, (), ()>(n * 10));
  let v = run_blocking(s.run_collect(), ()).expect("collect");
  assert_eq!(v, vec![10, 20, 30, 40]);
  println!("070_stream_map_par_n ok");
}
