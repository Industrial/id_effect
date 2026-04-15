//! Ex 022 — `Get::<K>::get` reads the head cell.
use id_effect::{Get, ctx, service_key};

service_key!(struct K);

fn main() {
  let env = ctx!(K => "here");
  assert_eq!(*Get::<K>::get(&env), "here");
  println!("022_get_here ok");
}
