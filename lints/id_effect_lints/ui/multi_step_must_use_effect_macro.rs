// B-01: two chained `.flat_map` calls.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

impl<X, Y, Z> Effect<X, Y, Z> {
  fn flat_map<B>(self, _f: impl FnOnce(X) -> Effect<B, Y, Z>) -> Effect<B, Y, Z> {
    Effect(PhantomData)
  }
}

fn step(_n: u8) -> Effect<u8, (), ()> {
  Effect(PhantomData)
}

fn two_steps() -> Effect<u8, (), ()> {
  step(0).flat_map(|a| step(a)).flat_map(|b| step(b))
}

fn one_step() -> Effect<u8, (), ()> {
  step(0).flat_map(|a| step(a))
}

fn main() {}
