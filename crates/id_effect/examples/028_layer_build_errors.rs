//! Ex 028 — Failed layers propagate `Err` from `build`.
use id_effect::{Layer, LayerFn, Tagged, service_key};

service_key!(struct K);

fn main() {
  let bad = LayerFn(|| Err::<Tagged<K, u8>, &'static str>("no"));
  assert_eq!(bad.build(), Err("no"));
  println!("028_layer_build_errors ok");
}
