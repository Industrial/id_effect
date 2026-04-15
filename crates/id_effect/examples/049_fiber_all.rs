//! Ex 049 — `fiber_all` joins completed [`FiberHandle`]s in order.
use id_effect::{FiberId, fiber_all, fiber_succeed, run_blocking};

fn main() {
  let a = fiber_succeed::<i32, ()>(FiberId::fresh(), 10_i32);
  let b = fiber_succeed::<i32, ()>(FiberId::fresh(), 20_i32);
  let out = run_blocking(fiber_all(vec![a, b]), ()).expect("fiber_all");
  assert_eq!(out, vec![10, 20]);
  println!("049_fiber_all ok");
}
