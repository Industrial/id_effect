// A-05: a 0.6 `Env` as `R` in a public signature.
#![allow(dead_code)]
use std::marker::PhantomData;

pub struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);
pub struct Env;

pub fn old_env() -> Effect<u8, (), Env> {
  Effect(PhantomData)
}

pub fn no_env() -> Effect<u8, (), ()> {
  Effect(PhantomData)
}

fn main() {}
