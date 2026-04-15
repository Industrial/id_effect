//! Ex 026 — `LayerFn` builds one layer cell.
use id_effect::{Layer, LayerFn, Tagged, service_key};

service_key!(struct SeedKey);

fn main() {
  let layer = LayerFn(|| Ok::<Tagged<SeedKey, u32>, ()>(Tagged::<SeedKey, _>::new(42_u32)));
  let cell = layer.build().expect("layer");
  assert_eq!(cell.value, 42);
  println!("026_layer_fn ok");
}
