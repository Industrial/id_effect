//! Ex 019 — `Tagged` associates a key type with a runtime value.
use id_effect::{Tagged, service_key};

service_key!(struct TokenKey);

fn main() {
  let cell = Tagged::<TokenKey, _>::new("abc");
  assert_eq!(cell.value, "abc");
  println!("019_tagged_cell ok");
}
