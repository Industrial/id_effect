//! Bifold an Either.

use id_effect::algebra::bifoldable::either::bifold;
use id_effect::foundation::coproduct::{left, right};

fn main() {
  let l = bifold(
    left::<String, i32>(1),
    |x| format!("L{x}"),
    |x| format!("R{x}"),
  );
  let r = bifold(
    right::<String, i32>("ok".into()),
    |x| format!("L{x}"),
    |x| format!("R{x}"),
  );
  println!("{l} / {r}");
}
