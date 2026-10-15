//! Ex 070 — `map_effect_par(n, f)` runs up to `n` element effects at once, keeping order.
use id_effect::Exit;
use id_effect::{Stream, succeed};

fn main() {
  let s = Stream::<i32, ()>::from_iterable([1, 2, 3, 4]).map_effect_par(2, |n| succeed(n * 10));
  assert_eq!(
    s.run_collect().block_on(),
    Exit::succeed(vec![10, 20, 30, 40])
  );
  println!("070_stream_map_par_n ok");
}
