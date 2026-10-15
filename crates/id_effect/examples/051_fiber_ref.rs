//! Ex 051 — `FiberRef` stores a fiber-local value.
use id_effect::Exit;
use id_effect::next::{Effect, FiberRef};

fn main() {
  let r = FiberRef::new(0_u32);
  let r2 = r.clone();
  let program: Effect<u32, ()> = r.set(42).flat_map(move |()| r2.get());
  assert_eq!(program.block_on(), Exit::succeed(42));
  // `locally` scopes a value to one effect.
  let scoped: Effect<u32, ()> = r.locally(7, r.get());
  assert_eq!(scoped.block_on(), Exit::succeed(7));
  println!("051_fiber_ref ok");
}
