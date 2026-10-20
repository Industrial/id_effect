// run-rustfix
// L-02: `.flat_map(|x| succeed(…))` on an `Effect` (MachineApplicable: `.map(|x| …)`).
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

impl<X, Y, Z> Effect<X, Y, Z> {
  fn map<B>(self, _f: impl FnOnce(X) -> B) -> Effect<B, Y, Z> {
    Effect(PhantomData)
  }
  fn flat_map<B>(self, _f: impl FnOnce(X) -> Effect<B, Y, Z>) -> Effect<B, Y, Z> {
    Effect(PhantomData)
  }
}

fn succeed<X, Y, Z>(_x: X) -> Effect<X, Y, Z>
where
  X: Sized,
{
  Effect(PhantomData)
}

fn load() -> Effect<u8, (), ()> {
  Effect(PhantomData)
}

fn doubled() -> Effect<u16, (), ()> {
  load().flat_map(|n| succeed(u16::from(n) * 2))
}

fn moved() -> Effect<u8, (), ()> {
  load().flat_map(move |n| succeed(n + 1))
}

// A turbofish may be the only annotation: left alone.
fn annotated() -> Effect<u32, (), ()> {
  load().flat_map(|_| succeed::<u32, (), ()>(0))
}

fn real_flat_map() -> Effect<u8, (), ()> {
  load().flat_map(|_| load())
}

fn main() {}
