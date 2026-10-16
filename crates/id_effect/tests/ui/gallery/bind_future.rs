// Gallery: `~` on a future; `~` binds effects only. Fix: `.await` it inside the body.
use id_effect::Never;
use id_effect::{Effect, effect};

async fn fetch() -> u32 {
  7
}

fn handler() -> Effect<u32, Never> {
  effect!({ ~fetch() })
}

fn main() {
  let _ = handler();
}
