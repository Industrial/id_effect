// (3) Providing a capability the effect never needed.
use id_effect::Never;
use id_effect::next::{Effect, caps_next, effect_next};

pub struct Greeting(pub &'static str);

#[derive(Default)]
pub struct Cache;

fn hello() -> Effect<String, Never, caps_next!(Greeting)> {
  effect_next!({ ~Greeting.0.to_string() })
}

fn main() {
  let _ = hello().provide(Cache::default());
}
