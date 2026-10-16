// Gallery: `.provide(..)` of a capability the effect never asked for.
// Fix: drop the `.provide(Cache)` (or add `Cache` to the effect's `caps!(..)` if it reads it).
use id_effect::Never;
use id_effect::{Effect, caps, effect};

pub struct Greeting(pub &'static str);

pub struct Logger;

impl Logger {
  pub fn info(&self, _line: &str) {}
}

#[derive(Default)]
pub struct Cache;

fn hello() -> Effect<String, Never, caps!(Greeting, Logger)> {
  effect!({
    ~Logger.info("hello");
    ~Greeting.0.to_string()
  })
}

fn main() {
  let _ = hello().provide(Cache::default());
}
