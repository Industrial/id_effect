// D-01: `block_on` outside `main`.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

impl<X, Y, Z> Effect<X, Y, Z> {
  fn block_on(self) {}
}

fn ready() -> Effect<u8, (), ()> {
  Effect(PhantomData)
}

fn library_helper() {
  ready().block_on();
}

fn main() {
  ready().block_on();
}
