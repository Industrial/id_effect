// J-01: `println!` / `eprintln!` in an Effect-returning fn.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

fn noisy() -> Effect<u8, (), ()> {
  println!("step 1");
  eprintln!("step 2");
  Effect(PhantomData)
}

fn cli() {
  println!("fine outside Effect code");
}

fn main() {}
