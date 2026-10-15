//! Ex 106 — `TokioRuntime` runs `next` effects on the Tokio driver (sleep / yield on tokio timers).
//!
//! Run: `cargo run -p id_effect_tokio --example 106_tokio_runtime`

use std::time::Duration;

use id_effect::Exit;
use id_effect::next::{HNil, sleep, succeed, yield_fiber};
use id_effect_tokio::TokioRuntime;

fn main() {
  let rt = TokioRuntime::new_current_thread().expect("tokio runtime should build");
  rt.block_on(async {
    assert!(matches!(
      rt.run(sleep::<(), HNil>(Duration::from_millis(0))).await,
      Exit::Success(())
    ));
    assert!(matches!(
      rt.run(yield_fiber::<(), HNil>()).await,
      Exit::Success(())
    ));
    assert!(matches!(
      rt.run(succeed::<u8, (), HNil>(42)).await,
      Exit::Success(42)
    ));
  });
  println!("106_tokio_runtime ok");
}
