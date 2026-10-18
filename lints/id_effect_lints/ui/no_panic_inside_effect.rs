// F-04: `panic!` / `unreachable!` in an Effect-returning fn.
#![allow(dead_code, unreachable_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

fn explode(flag: bool) -> Effect<u8, (), ()> {
  if flag {
    panic!("boom");
  }
  unreachable!()
}

fn plain() {
  panic!("fine outside Effect code");
}

fn main() {}
