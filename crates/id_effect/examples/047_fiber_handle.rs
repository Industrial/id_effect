//! Ex 047 — `FiberHandle` tracks completion / interruption.
use id_effect::{FiberHandle, FiberId, FiberStatus};

fn main() {
  let h = FiberHandle::<u8, ()>::pending(FiberId::fresh());
  assert_eq!(h.status(), FiberStatus::Running);
  h.mark_completed(Ok(7));
  assert_eq!(pollster::block_on(h.join()), Ok(7));
  println!("047_fiber_handle ok");
}
