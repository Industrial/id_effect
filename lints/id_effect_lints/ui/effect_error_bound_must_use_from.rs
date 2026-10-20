// A-04: `E` without an `E: From<_>` bound.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

fn no_from<A, E>() -> Effect<A, E, ()>
where
  A: Send,
  E: Send,
{
  Effect(PhantomData)
}

fn with_from<A, E>() -> Effect<A, E, ()>
where
  A: Send,
  E: From<std::io::Error>,
{
  Effect(PhantomData)
}

fn main() {}
