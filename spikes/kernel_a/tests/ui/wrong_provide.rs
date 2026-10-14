// (3) Providing a capability the effect never needed.
use kernel_a::{Effect, Never, caps, effect};
use spike_corpus::{Cache, Greeting};

fn hello() -> Effect<String, Never, caps!(Greeting)> {
  effect!({ ~Greeting.0.to_string() })
}

fn main() {
  let _ = hello().provide(Cache::default());
}
