// F-03: `.unwrap()` / `.expect()` in an Effect-returning fn.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

fn parse(s: &str) -> Effect<u8, (), ()> {
  let _n: u8 = s.parse().unwrap();
  let _m: u8 = s.parse().expect("a number");
  Effect(PhantomData)
}

// Outside Effect code `unwrap` is not this lint's business.
fn plain(s: &str) -> u8 {
  s.parse().unwrap()
}

fn main() {}
