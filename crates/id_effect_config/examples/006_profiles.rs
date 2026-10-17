//! `Profiles`: `config/base.toml` → `config/<profile>.toml` → `APP_*` environment.
//!
//! Run: `cargo run -p id_effect_config --example 006_profiles -- --profile prod`
//! (or `ID_EFFECT_PROFILE=prod`; the default profile is `dev`).

use id_effect::Exit;
use id_effect::{Effect, HCons, HNil, need};
use id_effect_config::{Config, ConfigError, Profiles};

/// The application's server settings, provided as a capability.
#[derive(Debug)]
struct ServerConfig {
  host: String,
  port: i64,
}

fn server_config() -> Config<ServerConfig> {
  Config::all((Config::string("host"), Config::integer("port")))
    .map(|(host, port)| ServerConfig { host, port })
}

fn describe() -> Effect<String, ConfigError, HCons<ServerConfig, HNil>> {
  need::<ServerConfig>().map(|c| format!("listening on {}:{}", c.host, c.port))
}

fn main() {
  // A throwaway `config/` directory standing in for the one shipped with the application.
  let root = tempfile::tempdir().expect("tempdir");
  let dir = root.path().join("config");
  std::fs::create_dir(&dir).expect("config dir");
  std::fs::write(dir.join("base.toml"), "host = \"127.0.0.1\"\nport = 3000\n").expect("base");
  std::fs::write(dir.join("prod.toml"), "host = \"0.0.0.0\"\nport = 8080\n").expect("prod");

  let profiles = Profiles::new().dir(&dir);
  let profile = match profiles.resolve() {
    Ok(profile) => profile,
    Err(e) => {
      eprintln!("{e}");
      std::process::exit(2);
    }
  };
  println!("profile: {profile}");

  // Profile layer (the `ConfigProviderService` capability) → config layer (`ServerConfig`).
  let app = profiles
    .layer::<ConfigError>()
    .provide_to(server_config().layer());
  match describe().provide_layer(app).block_on() {
    Exit::Success(line) => println!("{line}"),
    Exit::Failure(cause) => panic!("config failed to load: {cause:?}"),
  }
}
