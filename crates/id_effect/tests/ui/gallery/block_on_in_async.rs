// Gallery: `.block_on()` inside async code (it parks the thread, and its `Exit` is not a
// future). Fix: `Runtime::global().run(job()).await`.
use id_effect::Never;
use id_effect::{Effect, succeed};

fn job() -> Effect<u32, Never> {
  succeed(1)
}

async fn serve() -> u32 {
  match job().block_on().await {
    _ => 0,
  }
}

fn main() {
  let _ = serve();
}
