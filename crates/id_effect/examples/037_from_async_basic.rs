//! Ex 037 — `from_async` wraps a real `async` step.
use id_effect::Exit;
use id_effect::next::{Effect, from_async};

fn main() {
  let eff: Effect<u8, ()> = from_async(|| async move {
    core::future::ready(()).await;
    Ok(42)
  });
  assert_eq!(eff.block_on(), Exit::succeed(42));
  println!("037_from_async_basic ok");
}
