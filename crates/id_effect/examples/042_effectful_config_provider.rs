//! Ex 042 — an effectful `Layer` builds the config before the app runs.
use id_effect::{Effect, HCons, HNil, Layer, caps, effect, hlist, sync};
use id_effect::{Exit, Never};

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppConfig(String);

fn config_layer() -> Layer<HCons<AppConfig, HNil>, Never> {
  Layer::effect(|| sync(|| hlist![AppConfig("loaded-from-effect".to_string())]))
}

fn app() -> Effect<String, Never, caps!(AppConfig)> {
  effect!({ ~AppConfig.0.clone() })
}

fn main() {
  let cfg = app().provide_layer(config_layer()).block_on();
  assert_eq!(cfg, Exit::succeed("loaded-from-effect".to_string()));
  println!("042_effectful_config_provider ok");
}
