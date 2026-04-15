//! Ex 023 — `get_path` follows `ThereHere` / `Skip` paths.
use id_effect::{ThereHere, ctx, service_key};

service_key!(struct FirstKey);
service_key!(struct SecondKey);

fn main() {
  let env = ctx!(FirstKey => 1_u8, SecondKey => 2_u16);
  assert_eq!(*env.get_path::<SecondKey, ThereHere>(), 2);
  println!("023_get_path ok");
}
