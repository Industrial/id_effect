//! Ex 087 — `EffectData` enables structural equality/hash for domain values.
use id_effect::EffectData;

#[derive(Clone, Debug, EffectData)]
struct Point {
  x: i32,
  y: i32,
}

fn main() {
  let a = Point { x: 1, y: 2 };
  let b = Point { x: 1, y: 2 };
  assert_eq!(a, b);
  println!("087_effect_data ok");
}
