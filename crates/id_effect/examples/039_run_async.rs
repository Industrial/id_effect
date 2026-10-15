//! Ex 039 — Inside an async program, `Runtime::run` awaits an effect instead of blocking.
use id_effect::Exit;
use id_effect::next::{Effect, Runtime, succeed};

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let eff: Effect<u8, ()> = succeed(9);
  assert_eq!(Runtime::global().run(eff).await, Exit::succeed(9));
  println!("039_run_async ok");
}
