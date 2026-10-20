// A-07: an Effect-returning fn named `*_effect`.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

fn load_user_effect() -> Effect<u8, (), ()> {
  Effect(PhantomData)
}

fn load_user() -> Effect<u8, (), ()> {
  Effect(PhantomData)
}

fn main() {}
