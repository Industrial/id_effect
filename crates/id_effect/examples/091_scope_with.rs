//! Ex 091 — `Effect::scoped` hands the body a `Scope`; its finalizers run LIFO when it closes.
use std::sync::{Arc, Mutex};

use id_effect::Exit;
use id_effect::{Effect, succeed, sync};

fn main() {
  let order = Arc::new(Mutex::new(Vec::new()));
  let o = Arc::clone(&order);
  let eff: Effect<u8, &'static str> = Effect::scoped(move |scope| {
    let o1 = Arc::clone(&o);
    let o2 = Arc::clone(&o);
    scope
      .add_finalizer(sync(move || o1.lock().unwrap().push(1_u8)))
      .then(scope.add_finalizer(sync(move || o2.lock().unwrap().push(2_u8))))
      .then(succeed(0))
  });
  assert_eq!(eff.block_on(), Exit::succeed(0));
  assert_eq!(*order.lock().unwrap(), vec![2, 1]);
  println!("091_scope_with ok");
}
