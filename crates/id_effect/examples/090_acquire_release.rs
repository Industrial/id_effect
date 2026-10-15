//! Ex 090 — `acquire_use_release` runs the release after the use, whatever its outcome.
use std::sync::{Arc, Mutex};

use id_effect::Exit;
use id_effect::{Effect, ExitKind, acquire_use_release, succeed, sync};

fn main() {
  let log = Arc::new(Mutex::new(Vec::new()));
  let log2 = Arc::clone(&log);
  let eff: Effect<u8, &'static str> =
    acquire_use_release(succeed(9_u8), succeed, move |n, _kind: ExitKind| {
      sync(move || log2.lock().unwrap().push(n))
    });
  assert_eq!(eff.block_on(), Exit::succeed(9));
  assert_eq!(*log.lock().unwrap(), vec![9]);
  println!("090_acquire_release ok");
}
