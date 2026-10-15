// (2a) A duplicate written by hand in `caps!(..)`: caught by the macro (by token), with one
// error at the second occurrence and no cascading `Selector` error.
use id_effect::Never;
use id_effect::{Effect, caps, effect};

pub struct Db;

pub struct ConfigSource;

impl ConfigSource {
  pub fn get_string(&self, _key: &str) -> Option<String> {
    None
  }
}

fn handler() -> Effect<String, Never, caps!(ConfigSource, Db, ConfigSource)> {
  effect!({ ~ConfigSource.get_string("x").unwrap_or_default() })
}

fn main() {
  let _ = handler();
}
