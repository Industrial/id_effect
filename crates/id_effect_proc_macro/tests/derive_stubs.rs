use id_effect::schema::{HasSchema, Unknown};
use id_effect_proc_macro::{EffectData, Optics, Schema};

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

#[derive(Clone, Debug, PartialEq, Schema)]
struct Account {
  #[schema("account_id")]
  id: i64,
  tags: Vec<String>,
  note: Option<String>,
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
  assert_eq!(User { name: "ada".into() }.name, "ada");
  use std::collections::hash_map::DefaultHasher;
  use std::hash::{Hash, Hasher};
  let mut h = DefaultHasher::new();
  User { name: "bob".into() }.hash(&mut h);
  assert_ne!(h.finish(), 0);
}

#[test]
fn derive_schema_round_trips_with_wire_rename() {
  let account = Account {
    id: 7,
    tags: vec!["a".into()],
    note: None,
  };
  let wire = Account::schema().encode(account.clone());
  let Unknown::Object(map) = &wire else {
    panic!("expected object wire, got {wire:?}");
  };
  assert_eq!(map.get("account_id"), Some(&Unknown::I64(7)));
  assert_eq!(
    <Account as HasSchema>::schema()
      .decode_unknown(&wire)
      .unwrap(),
    account
  );
}
