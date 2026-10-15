// (3) Providing a capability the effect never needed.
use id_effect::Never;
use id_effect::{Effect, caps, effect};

pub struct Greeting(pub &'static str);

#[derive(Default)]
pub struct Cache;

fn hello() -> Effect<String, Never, caps!(Greeting)> {
  effect!({ ~Greeting.0.to_string() })
}

fn main() {
  let _ = hello().provide(Cache::default());
}
