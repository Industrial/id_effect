//! Ex 037 — `Effect::new_async` wraps an async body (driven by `run_async` / runtime).
use id_effect::{Effect, box_future, run_async};

fn main() {
  let eff = Effect::new_async(|_r: &mut ()| {
    box_future(async move {
      core::future::ready(()).await;
      Ok::<u8, ()>(42_u8)
    })
  });
  assert_eq!(pollster::block_on(run_async(eff, ())), Ok(42));
  println!("037_from_async_basic ok");
}
