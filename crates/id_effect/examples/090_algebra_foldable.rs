//! Foldable and Alternative over Option.

use id_effect::algebra::alternative::option::alt;
use id_effect::algebra::foldable::option::fold_right;

fn main() {
  let v = fold_right(Some(10), 0, |a, b| a + b);
  let c = alt(None, Some(42));
  println!("fold={v} alt={c:?}");
}
