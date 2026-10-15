//! Ex 049 — `all` runs effects on child fibers and collects their values in order.
use id_effect::Exit;
use id_effect::next::{Concurrency, Effect, all, succeed};

fn main() {
  let work: Vec<Effect<i32, ()>> = vec![succeed(10), succeed(20)];
  let out = all(work, Concurrency::Unbounded).block_on();
  assert_eq!(out, Exit::succeed(vec![10, 20]));
  println!("049_fiber_all ok");
}
