//! Ex 108 — `TokioRuntime` on an existing tokio runtime: monotonic `now`, async `sleep`,
//! cooperative `yield_fiber`.
//!
//! Run: `cargo run -p id_effect_tokio --example 108_tokio_clock`

use std::time::Duration;

use id_effect::Exit;
use id_effect::next::{HNil, sleep, yield_fiber};
use id_effect_tokio::TokioRuntime;

fn main() {
  let tokio_rt = tokio::runtime::Builder::new_current_thread()
    .enable_time()
    .build()
    .expect("tokio runtime should build");
  let rt = TokioRuntime::from_handle(tokio_rt.handle().clone());
  tokio_rt.block_on(async {
    let t1 = rt.now();
    let t2 = rt.now();
    assert!(t2 >= t1);

    let nap = sleep::<(), HNil>(Duration::from_millis(1)).then(yield_fiber());
    assert!(matches!(rt.run(nap).await, Exit::Success(())));
    assert!(rt.now() >= t1 + Duration::from_millis(1));
  });

  println!("108_tokio_clock ok");
}
