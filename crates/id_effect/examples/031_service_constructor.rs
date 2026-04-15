//! Ex 031 — `service` wraps a value as `Tagged<K, V>`.
use id_effect::{service, service_key};

service_key!(struct PortKey);

fn main() {
  let cell = service::<PortKey, _>(8080_u16);
  assert_eq!(cell.value, 8080);
  println!("031_service_constructor ok");
}
