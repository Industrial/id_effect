//! Ex 029 — `service_key!` declares a nominal tag type.
use id_effect::service_key;

service_key!(pub struct ApiKey);

fn main() {
  let _ = std::any::TypeId::of::<ApiKey>();
  println!("029_service_key ok");
}
