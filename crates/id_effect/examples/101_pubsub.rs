//! Ex 101 — A `PubSub` hub: `subscribe` in a scope, then `publish`.
use id_effect::next::{Effect, PubSub, QueueError, Scope};

fn main() {
  let ps = PubSub::<u32>::bounded(8);
  let scope = Scope::new();
  let program: Effect<u32, QueueError> = ps.subscribe(&scope).flat_map({
    let ps = ps.clone();
    move |q| {
      ps.publish(41)
        .then(ps.publish(1))
        .then(q.take())
        .zip(q.take())
        .map(|(a, b)| a + b)
    }
  });
  assert_eq!(program.block_on().into_result().ok(), Some(42));
  println!("101_pubsub ok");
}
