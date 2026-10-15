//! [`MapConfigProvider`] with `Config<T>` descriptors (nested keys, defaults, `Config::all`).

use id_effect_config::{Config, ConfigError, MapConfigProvider};

fn main() -> Result<(), ConfigError> {
  let p = MapConfigProvider::from_pairs([("SERVICE_HOST", "localhost"), ("SERVICE_NAME", "demo")]);

  let service = Config::all((
    Config::string("HOST"),
    Config::string("NAME"),
    Config::integer("THREADS").with_default(4),
  ))
  .nested("SERVICE");
  let (host, name, threads) = service.load(&p)?;
  println!("host={host} name={name} threads={threads}");

  // `Config::all` reports every failure, not only the first.
  let empty = MapConfigProvider::from_pairs::<[(&str, &str); 0], _, _>([]);
  let err = service.load(&empty).unwrap_err();
  println!("{err}");
  assert_eq!(err.errors().len(), 2);
  Ok(())
}
