//! Build a [`FigmentConfigProvider`](id_effect_config::FigmentConfigProvider) via
//! [`provide_figment_config_provider`](id_effect_config::provide_figment_config_provider).

use id_effect::{build_env, run_blocking};
use id_effect_config::{Config, ConfigError, figment, provide_figment_config_provider};

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let dir = tempfile::tempdir()?;
  let path = dir.path().join("c.toml");
  std::fs::write(&path, "app_name = \"example\"\n")?;

  let env = build_env([provide_figment_config_provider(figment::from_toml_file(
    &path,
  ))])
  .map_err(|e| format!("{e:?}"))?;
  let name: String = run_blocking(
    Config::string("app_name").run::<String, ConfigError, _>(),
    env,
  )?;
  println!("app_name={name}");
  Ok(())
}
