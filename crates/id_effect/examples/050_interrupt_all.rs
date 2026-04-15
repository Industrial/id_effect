//! Ex 050 — `interrupt_all` signals every handle.
use id_effect::{FiberHandle, FiberId, interrupt_all, run_blocking};

fn main() {
  let a = FiberHandle::<(), ()>::pending(FiberId::fresh());
  let b = FiberHandle::<(), ()>::pending(FiberId::fresh());
  let _ = run_blocking(interrupt_all(vec![a.clone(), b.clone()]), ());
  assert!(a.is_done());
  assert!(b.is_done());
  println!("050_interrupt_all ok");
}
