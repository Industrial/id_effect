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

  // A prism leaves non-matching variants untouched.
  let rect = circle.modify(Shape::Rect { w: 2.0, h: 3.0 }, |r| r * 3.0);
  if let Shape::Rect { w, h } = rect {
    println!("rect unchanged: {w} x {h}");
  }
}
