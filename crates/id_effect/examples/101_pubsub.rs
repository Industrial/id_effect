//! Ex 101 — `PubSub` broadcast hub with `publish` / `subscribe`.
use id_effect::{PubSub, Scope, run_async};

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let ps = run_async(PubSub::<u32>::bounded(8), ())
    .await
    .expect("pubsub");
  let scope = Scope::make();
  let q = run_async(ps.subscribe(), scope.clone())
    .await
    .expect("subscribe");
  assert!(run_async(ps.publish(41), ()).await.expect("pub"));
  assert!(run_async(ps.publish(1), ()).await.expect("pub"));
  let a = run_async(q.take(), ()).await.expect("take");
  let b = run_async(q.take(), ()).await.expect("take");
  assert_eq!(a + b, 42);
  println!("101_pubsub ok");
}
