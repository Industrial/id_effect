//! Traverse a vector with Effect.

use id_effect::algebra::traversable::traverse_vec;
use id_effect::runtime::run_blocking;
use id_effect::succeed;

fn main() {
  let eff = traverse_vec(vec![1, 2, 3], |n| succeed::<i32, (), ()>(n * n));
  println!("{:?}", run_blocking(eff, ()));
}
