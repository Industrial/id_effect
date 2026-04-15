//! Ex 100 — Duplex `Channel` backed by a `Queue` (write / read outside `Stream`).
use id_effect::{Channel, run_blocking};

fn main() {
  let ch = run_blocking(Channel::<i32, i32, (), (), ()>::duplex_unbounded(), ()).expect("channel");
  run_blocking(ch.write(21), ()).expect("write");
  run_blocking(ch.write(21), ()).expect("write");
  let a = run_blocking(ch.read(), ()).expect("read").expect("elem");
  let b = run_blocking(ch.read(), ()).expect("read").expect("elem");
  assert_eq!(a + b, 42);
  println!("100_channel_queue ok");
}
