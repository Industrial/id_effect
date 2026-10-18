// G-01: a discarded `.fork()`.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);
struct FiberHandle;

impl<X, Y, Z> Effect<X, Y, Z> {
  fn fork(self) -> FiberHandle {
    FiberHandle
  }
}

impl FiberHandle {
  fn join(self) {}
}

fn work() -> Effect<u8, (), ()> {
  Effect(PhantomData)
}

fn fire_and_forget() {
  work().fork();
}

fn joined() {
  let handle = work().fork();
  handle.join();
}

fn main() {}
