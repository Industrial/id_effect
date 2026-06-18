//! **Traversable** — fold with an applicative effect.

use std::task::{Context, Poll, Waker};

use crate::kernel::Effect;

fn run_effect_sync<A, E, R>(effect: Effect<A, E, R>, env: &mut R) -> Result<A, E>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  let mut fut = effect.run(env);
  let waker = Waker::noop();
  let mut cx = Context::from_waker(waker);
  loop {
    match fut.as_mut().poll(&mut cx) {
      Poll::Ready(out) => return out,
      Poll::Pending => std::thread::yield_now(),
    }
  }
}

/// Traverse a vector with an effectful function (sequential bind).
pub fn traverse_vec<A, B, E, R, F>(xs: Vec<A>, mut f: F) -> Effect<Vec<B>, E, R>
where
  A: 'static,
  B: 'static,
  R: 'static,
  F: FnMut(A) -> Effect<B, E, R>,
  F: 'static,
{
  Effect::new(move |env| {
    let mut out = Vec::with_capacity(xs.len());
    for x in xs {
      out.push(run_effect_sync(f(x), env)?);
    }
    Ok(out)
  })
}

/// Sequence a vector of effects.
pub fn sequence_vec<A, E, R>(xs: Vec<Effect<A, E, R>>) -> Effect<Vec<A>, E, R>
where
  A: 'static,
  R: 'static,
{
  traverse_vec(xs, |eff| eff)
}

/// Traverse an optional value.
pub fn traverse_option<A, B, E, R, F>(opt: Option<A>, f: F) -> Effect<Option<B>, E, R>
where
  A: 'static,
  B: 'static,
  F: FnOnce(A) -> Effect<B, E, R>,
{
  match opt {
    Some(a) => f(a).map(Some),
    None => crate::succeed(None),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::run_blocking;

  #[test]
  fn traverse_vec_empty_input() {
    let eff = traverse_vec(Vec::<i32>::new(), |n| crate::succeed::<i32, (), ()>(n));
    assert_eq!(run_blocking(eff, ()), Ok(vec![]));
  }

  #[test]
  fn traverse_vec_collects() {
    let xs = vec![1, 2, 3];
    let eff = traverse_vec(xs, |n| crate::succeed::<i32, (), ()>(n * 2));
    assert_eq!(run_blocking(eff, ()), Ok(vec![2, 4, 6]));
  }

  #[test]
  fn sequence_vec_empty() {
    let xs: Vec<crate::Effect<i32, (), ()>> = vec![];
    assert_eq!(run_blocking(sequence_vec(xs), ()), Ok(vec![]));
  }

  #[test]
  fn sequence_vec_runs_all() {
    let xs: Vec<crate::Effect<i32, (), ()>> = vec![crate::succeed(1), crate::succeed(2)];
    assert_eq!(run_blocking(sequence_vec(xs), ()), Ok(vec![1, 2]));
  }

  #[test]
  fn traverse_option_some() {
    let eff = traverse_option(Some(4i32), |n| crate::succeed::<i32, (), ()>(n + 1));
    assert_eq!(run_blocking(eff, ()), Ok(Some(5)));
  }

  #[test]
  fn traverse_option_none() {
    let eff = traverse_option(None::<i32>, |n| crate::succeed::<i32, (), ()>(n));
    assert_eq!(run_blocking(eff, ()), Ok(None));
  }
}
