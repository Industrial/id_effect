//! Traverse a vector with Effect.

use id_effect::{Effect, succeed};
use id_effect_fp::algebra::traversable::traverse_vec;

fn main() {
  let eff: Effect<Vec<i32>, ()> = traverse_vec(vec![1, 2, 3], |n| succeed(n * n));
  println!("{:?}", eff.block_on());
}
