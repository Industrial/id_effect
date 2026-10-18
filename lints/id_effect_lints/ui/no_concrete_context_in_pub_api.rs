// A-05: a 0.6 `Context<Cons<…>>` as `R` in a public signature.
#![allow(dead_code)]
use std::marker::PhantomData;

pub struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);
pub struct Context<T>(PhantomData<T>);
pub struct Cons<H, T>(PhantomData<(H, T)>);
pub struct Nil;
pub struct HCons<H, T>(PhantomData<(H, T)>);
pub struct HNil;

pub fn old_env() -> Effect<u8, (), Context<Cons<u8, Nil>>> {
  Effect(PhantomData)
}

pub fn caps_env() -> Effect<u8, (), HCons<u8, HNil>> {
  Effect(PhantomData)
}

fn main() {}
