//! Ex 092 — `scoped` vs `scope_with`.
//!
//! - [`id_effect::scoped`] wraps an already-built [`Effect`]: a fresh [`Scope`] is created, the inner
//!   effect runs, then the scope is closed (success → [`Exit::succeed`], error → [`Exit::die`]).
//!   You do **not** get a [`Scope`] handle, so you cannot call [`Scope::add_finalizer`].
//! - [`id_effect::scope_with`] passes a [`Scope`] into your closure so you can register finalizers
//!   (LIFO on close). See `091_scope_with.rs`.
//!
//! Use `scoped` when you only need the uniform exit path around a subgraph. Use `scope_with` when
//! you need explicit finalizers.
use id_effect::{Exit, Never, run_blocking, scope_with, scoped, succeed};
use std::sync::{Arc, Mutex};

fn main() {
  let v = run_blocking(scoped(succeed::<i32, &'static str, ()>(99)), ()).expect("scoped succeed");
  assert_eq!(v, 99);

  let order = Arc::new(Mutex::new(Vec::new()));
  let o = Arc::clone(&order);
  let with_finalizer = scope_with(move |scope| {
    let o1 = Arc::clone(&o);
    scope.add_finalizer(Box::new(move |_e: Exit<(), Never>| {
      o1.lock().unwrap().push(1_u8);
      succeed::<(), Never, ()>(())
    }));
    succeed::<u8, &'static str, ()>(0)
  });
  assert_eq!(run_blocking(with_finalizer, ()), Ok(0));
  assert_eq!(*order.lock().unwrap(), vec![1_u8]);

  println!("092_scoped ok");
}
