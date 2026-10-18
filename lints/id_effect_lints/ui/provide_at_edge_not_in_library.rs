// E-04: `.provide(…)` in a library function.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

impl<X, Y, Z> Effect<X, Y, Z> {
  fn provide(self, _env: Z) -> Effect<X, Y, ()> {
    Effect(PhantomData)
  }
}

fn needs_env() -> Effect<u8, (), u32> {
  Effect(PhantomData)
}

fn library() -> Effect<u8, (), ()> {
  needs_env().provide(7)
}

fn main() {
  let _ = needs_env().provide(7);
}
