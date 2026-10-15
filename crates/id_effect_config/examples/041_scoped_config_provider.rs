//! Ex 041 — request-local config: an inner effect gets its own provider layer while the outer
//! program keeps the process-environment provider (replaces thread-local config overrides).

use id_effect::Exit;
use id_effect::next::Effect;
use id_effect_config::{
  Config, ConfigEnv, ConfigError, EnvConfigProviderLive, MapConfigProvider, provide_config_provider,
};

fn main() {
  // Request-local: this read sees only its own provider.
  let local: Effect<String, ConfigError> = Config::string("REGION")
    .run::<String, ConfigError, ConfigEnv, _>()
    .provide_layer(provide_config_provider(MapConfigProvider::from_pairs([(
      "REGION", "eu-west",
    )])));
  // The outer program runs against the process environment (`EnvConfigProviderLive`).
  let program: Effect<String, ConfigError, ConfigEnv> = local.narrow::<ConfigEnv, _>();
  let Exit::Success(region) = program
    .provide_layer(EnvConfigProviderLive::layer())
    .block_on()
  else {
    panic!("config read failed")
  };
  assert_eq!(region, "eu-west");
  println!("041_scoped_config_provider ok");
}
