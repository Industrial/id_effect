// (2a) A duplicate written by hand in `caps!(..)`: caught by the macro (by token).
use kernel_a::{Effect, Never, caps, effect};
use spike_corpus::{ConfigSource, Db};

fn handler() -> Effect<String, Never, caps!(ConfigSource, Db, ConfigSource)> {
  effect!({ ~ConfigSource.get_string("x").unwrap_or_default() })
}

fn main() {
  let _ = handler();
}
