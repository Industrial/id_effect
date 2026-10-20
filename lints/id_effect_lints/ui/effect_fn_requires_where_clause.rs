// A-03: a generic Effect-returning fn without a `where` clause.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

fn generic<A>(_a: A) -> Effect<A, (), ()> {
  Effect(PhantomData)
}

fn bounded<A>(_a: A) -> Effect<A, (), ()>
where
  A: Send,
{
  Effect(PhantomData)
}

fn main() {}
