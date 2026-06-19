//! Coverage for `match_effect!` macro expansion paths.

use id_effect::match_effect;

enum Color {
  Red,
  Green,
}

enum Shape {
  Circle(f64),
  Rect { w: f64, h: f64 },
}

#[test]
fn match_effect_unit_variants() {
  let green = match_effect!(Color, Color::Green, {
    Red => "red",
    Green => "green",
  });
  assert_eq!(green, "green");
  let red = match_effect!(Color, Color::Red, {
    Red => "red",
    Green => "green",
  });
  assert_eq!(red, "red");
}

#[test]
fn match_effect_tuple_and_struct_variants() {
  let circle = Shape::Circle(3.5);
  let r = match_effect!(Shape, circle, {
    Circle(r) => r,
    Rect { w, .. } => w,
  });
  assert!((r - 3.5).abs() < f64::EPSILON);

  let rect = Shape::Rect { w: 2.0, h: 4.0 };
  let (w, h) = match_effect!(Shape, rect, {
    Circle(r) => (r, r),
    Rect { w, h } => (w, h),
  });
  assert!((w - 2.0).abs() < f64::EPSILON);
  assert!((h - 4.0).abs() < f64::EPSILON);
}

enum PathVariant {
  Unit,
  Named(i32),
}

#[test]
fn match_effect_path_pattern() {
  let named = match_effect!(PathVariant, PathVariant::Named(9), {
    Named(n) => n,
    Unit => 0,
  });
  assert_eq!(named, 9);
  let unit = match_effect!(PathVariant, PathVariant::Unit, {
    Named(n) => n,
    Unit => -1,
  });
  assert_eq!(unit, -1);
}
