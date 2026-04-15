//! Ex 033 — `layer_service` builds a `Layer` for one tag.
use id_effect::{Layer, layer_service, service_key};

service_key!(struct IdKey);

fn main() {
  let layer = layer_service::<IdKey, _>(99_u64);
  let cell = layer.build().expect("build");
  assert_eq!(cell.value, 99);
  println!("033_layer_service ok");
}
