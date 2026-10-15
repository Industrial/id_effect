//! Ex 007 — Run a unit effect for its effect with `~expr;`.
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use id_effect::Exit;
use id_effect::{Effect, effect, sync};

fn bump(n: &Arc<AtomicU32>) -> Effect<()> {
  let n = Arc::clone(n);
  sync(move || {
    n.fetch_add(1, Ordering::SeqCst);
  })
}

fn main() {
  let calls = Arc::new(AtomicU32::new(0));
  let c = Arc::clone(&calls);
  let program: Effect<i32> = effect!({
    ~bump(&c);
    ~bump(&c);
    42
  });
  assert_eq!(program.block_on(), Exit::succeed(42));
  assert_eq!(calls.load(Ordering::SeqCst), 2);
  println!("007_effect_macro_discard ok");
}
