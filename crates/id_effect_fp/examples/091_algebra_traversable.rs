//! Traverse a vector with Effect.

use id_effect::runtime::run_blocking;
use id_effect::succeed;
use id_effect_fp::algebra::traversable::traverse_vec;

fn main() {
  let eff = traverse_vec(vec![1, 2, 3], |n| succeed::<i32, (), ()>(n * n));
  println!("{:?}", run_blocking(eff, ()));
}
