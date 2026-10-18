// A-06: a 0.6 `Get<Key, …>` bound.
#![allow(dead_code)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);
trait Get<K, H> {}
trait Selector<K, I> {}
struct Key;
struct Here;

fn old_bound<R>() -> Effect<u8, (), R>
where
  R: Get<Key, Here>,
{
  Effect(PhantomData)
}

fn selector_bound<R, I>() -> Effect<u8, (), R>
where
  R: Selector<Key, I>,
{
  Effect(PhantomData)
}

fn main() {}
