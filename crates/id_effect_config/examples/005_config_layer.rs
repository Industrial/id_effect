//! `Config::layer()`: the loaded configuration as a capability, built from a provider layer.
//!
//! Run: `cargo run -p id_effect_config --example 005_config_layer`

use id_effect::Exit;
use id_effect::{Effect, HCons, HNil, need};
use id_effect_config::{Config, ConfigError, ConfigProvider, MapConfigProvider};

/// The application's server settings, provided as a capability.
#[derive(Debug)]
struct ServerConfig {
  host: String,
  port: i64,
}

fn server_config() -> Config<ServerConfig> {
  Config::all((
    Config::string("HOST").with_default("127.0.0.1".into()),
    Config::integer("PORT"),
  ))
  .map(|(host, port)| ServerConfig { host, port })
  .nested("SERVER")
}

/// The program needs the loaded config, not the provider.
fn describe() -> Effect<String, ConfigError, HCons<ServerConfig, HNil>> {
  need::<ServerConfig>().map(|c| format!("listening on {}:{}", c.host, c.port))
}

fn main() {
  let provider = MapConfigProvider::from_pairs([("SERVER_PORT", "8080")]);
  // Provider layer (the `ConfigProviderService` capability) → config layer (`ServerConfig`).
  let app = provider.layer().provide_to(server_config().layer());
  let Exit::Success(line) = describe().provide_layer(app).block_on() else {
    panic!("config failed to load")
  };
  println!("{line}");
  assert_eq!(line, "listening on 127.0.0.1:8080");
}
