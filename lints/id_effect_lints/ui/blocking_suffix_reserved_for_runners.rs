// A-08: a `*_blocking` fn that returns an `Effect` instead of running it.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

fn load_blocking() -> Effect<u8, (), ()> {
  Effect(PhantomData)
}

fn run_blocking() -> u8 {
  0
}

fn main() {}
