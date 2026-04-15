//! [`MapConfigProvider`] with [`effect_config::config`] helpers (nested keys, defaults).

use effect_config::MapConfigProvider;
use effect_config::config;

fn main() -> Result<(), effect_config::ConfigError> {
  let p = MapConfigProvider::from_pairs([("SERVICE_HOST", "localhost"), ("SERVICE_NAME", "demo")]);

  let host = config::nested_string(&p, "SERVICE", "HOST")?;
  let name = config::nested_string(&p, "SERVICE", "NAME")?;
  let threads = config::with_default(config::nested_integer(&p, "SERVICE", "THREADS"), 4)?;

  println!("host={host} name={name} threads={threads}");
  Ok(())
}
