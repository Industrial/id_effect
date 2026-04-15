//! Ex 051 — `FiberRef` stores fiber-local values.
use id_effect::{FiberRef, run_blocking};

fn main() {
  let program = FiberRef::make(|| 0_u32).flat_map(|r| {
    let r2 = r.clone();
    r.set(42).flat_map(move |_| r2.get())
  });
  assert_eq!(run_blocking(program, ()), Ok(42));
  println!("051_fiber_ref ok");
}
