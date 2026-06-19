use id_effect_proc_macro::{EffectData, Fsm, Optics};

#[derive(Optics)]
struct Point {
  x: i32,
  y: i32,
}

#[derive(Optics)]
enum Traffic {
  Red,
  Green(i32),
}

#[derive(Fsm)]
enum Light {
  Red,
  Green,
}

#[derive(EffectData)]
struct User {
  name: String,
}

#[test]
fn derive_stubs_compile() {
  let p = Point { x: 1, y: 2 };
  assert_eq!((p.x, p.y), (1, 2));
  assert_eq!(Point::x_lens().get(&p), 1);
  assert_eq!(Point::y_lens().set(p, 3).y, 3);
  assert_eq!(Traffic::green_prism().preview(&Traffic::Green(4)), Some(4));
  assert!(matches!(Traffic::Red, Traffic::Red));
  assert!(matches!(Light::Red, Light::Red));
  assert!(matches!(Light::Green, Light::Green));
  assert_eq!(User { name: "ada".into() }.name, "ada");
  use std::collections::hash_map::DefaultHasher;
  use std::hash::{Hash, Hasher};
  let mut h = DefaultHasher::new();
  User { name: "bob".into() }.hash(&mut h);
  assert_ne!(h.finish(), 0);
}
