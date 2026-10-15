//! Build a [`FigmentConfigProvider`](id_effect_config::FigmentConfigProvider) via the
//! [`provide_figment_config_provider`](id_effect_config::provide_figment_config_provider) layer.

use id_effect::Exit;
use id_effect_config::{Config, ConfigEnv, ConfigError, figment, provide_figment_config_provider};

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let dir = tempfile::tempdir()?;
  let path = dir.path().join("c.toml");
  std::fs::write(&path, "app_name = \"example\"\n")?;

  let program = Config::string("app_name")
    .run::<String, ConfigError, ConfigEnv, _>()
    .provide_layer(provide_figment_config_provider(figment::from_toml_file(
      &path,
    )));
  match program.block_on() {
    Exit::Success(name) => println!("app_name={name}"),
    Exit::Failure(cause) => {
      return Err(match cause.fail_option() {
        Some(e) => e.into(),
        None => "config read did not complete".into(),
      });
    }
  }
  Ok(())
}
