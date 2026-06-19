//! Prism focus on enum variants.

use id_effect_optics::Prism;

#[derive(Clone, Debug, PartialEq)]
enum Shape {
  Circle(f64),
  Rect { w: f64, h: f64 },
}

fn main() {
  let circle = Prism::new(
    |s: &Shape| match s {
      Shape::Circle(r) => Some(*r),
      Shape::Rect { .. } => None,
    },
    Shape::Circle,
  );

  let updated = circle.modify(Shape::Circle(2.0), |r| r * 3.0);
  println!("{updated:?}");
}
