use id_effect_proc_macro::{Fsm, Optics, SchemaParser};

#[derive(Optics)]
struct Point {
  x: i32,
  y: i32,
}

#[derive(Fsm)]
enum Traffic {
  Red,
  Green,
}

#[derive(SchemaParser)]
struct User {
  name: String,
}

#[test]
fn derive_stubs_compile() {
  let p = Point { x: 1, y: 2 };
  assert_eq!((p.x, p.y), (1, 2));
  assert!(matches!(Traffic::Red, Traffic::Red));
  assert!(matches!(Traffic::Green, Traffic::Green));
  assert_eq!(User { name: "ada".into() }.name, "ada");
}
