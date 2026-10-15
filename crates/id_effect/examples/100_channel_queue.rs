//! Ex 100 — A duplex `Channel` backed by a `Queue` (write / read outside a `Stream`).
use id_effect::{Channel, Effect};

fn main() {
  let ch = Channel::<i32>::duplex_unbounded();
  let program: Effect<i32, ()> = ch
    .write(21)
    .then(ch.write(21))
    .then(ch.read())
    .zip(ch.read())
    .map(|(a, b)| a.expect("elem") + b.expect("elem"));
  assert_eq!(program.block_on().into_result().ok(), Some(42));
  println!("100_channel_queue ok");
}
