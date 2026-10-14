// (3b) Providing a capability the effect never needed, with a 2-capability `R` (with a single
// capability rustc instead commits to the only impl and reports E0308: see wrong_provide.rs).
use kernel_a::{Effect, Never, caps, effect};
use spike_corpus::{Cache, Greeting, Logger};

fn hello() -> Effect<String, Never, caps!(Greeting, Logger)> {
  effect!({
    ~Logger.info("hello");
    ~Greeting.0.to_string()
  })
}

fn main() {
  let _ = hello().provide(Cache::default());
}
