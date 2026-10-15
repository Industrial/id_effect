// (3b) Providing a capability the effect never needed, with a 2-capability `R` (with a single
// capability rustc may commit to the only impl instead: see wrong_provide.rs).
use id_effect::Never;
use id_effect::next::{Effect, caps_next, effect_next};

pub struct Greeting(pub &'static str);

pub struct Logger;

impl Logger {
  pub fn info(&self, _line: &str) {}
}

#[derive(Default)]
pub struct Cache;

fn hello() -> Effect<String, Never, caps_next!(Greeting, Logger)> {
  effect_next!({
    ~Logger.info("hello");
    ~Greeting.0.to_string()
  })
}

fn main() {
  let _ = hello().provide(Cache::default());
}
