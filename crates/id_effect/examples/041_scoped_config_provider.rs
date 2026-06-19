//! Ex 041 — request-local config via [`Env::scoped`] (replaces thread-local config overrides).

use id_effect::{build_env, provide, run_blocking};
use id_effect_config::{
  Config, ConfigError, EnvConfigProviderLive, MapConfigProvider, provide_config_provider,
};

fn main() {
  let base = build_env([provide!(EnvConfigProviderLive)]).expect("base env");
  let local = base
    .scoped([provide_config_provider(MapConfigProvider::from_pairs([(
      "REGION", "eu-west",
    )]))])
    .expect("scoped env");
  let region: String = run_blocking(
    Config::string("REGION").run::<String, ConfigError, _>(),
    local,
  )
  .unwrap();
  assert_eq!(region, "eu-west");
  println!("041_scoped_config_provider ok");
}
