//! Ex 020 — `Cons` / `Nil` heterogenous lists form environments.
use id_effect::{Cons, Nil, Tagged};

id_effect::service_key!(struct AKey);
id_effect::service_key!(struct BKey);

fn main() {
  let row = Cons(
    Tagged::<AKey, _>::new(1_u8),
    Cons(Tagged::<BKey, _>::new(2_u16), Nil),
  );
  assert_eq!(row.0.value, 1);
  assert_eq!(row.1.0.value, 2);
  println!("020_cons_nil ok");
}
