//! Ex 091 — `scope_with` runs finalizers in LIFO order.
use id_effect::{Exit, Never, run_blocking, scope_with, succeed};
use std::sync::{Arc, Mutex};

fn main() {
  let order = Arc::new(Mutex::new(Vec::new()));
  let o = Arc::clone(&order);
  let eff = scope_with(move |scope| {
    let o1 = Arc::clone(&o);
    scope.add_finalizer(Box::new(move |_e: Exit<(), Never>| {
      o1.lock().unwrap().push(1_u8);
      succeed::<(), Never, ()>(())
    }));
    let o2 = Arc::clone(&o);
    scope.add_finalizer(Box::new(move |_e: Exit<(), Never>| {
      o2.lock().unwrap().push(2_u8);
      succeed::<(), Never, ()>(())
    }));
    succeed::<u8, &'static str, ()>(0)
  });
  assert_eq!(run_blocking(eff, ()), Ok(0));
  assert_eq!(*order.lock().unwrap(), vec![2, 1]);
  println!("091_scope_with ok");
}
