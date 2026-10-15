//! **Traversable** — fold with an applicative effect.

use id_effect::{Effect, succeed, suspend};

/// Runs `f` on each remaining element in order, collecting into `acc` (sequential bind).
fn traverse_from<A, B, E, R, F>(
  mut it: std::vec::IntoIter<A>,
  mut f: F,
  mut acc: Vec<B>,
) -> Effect<Vec<B>, E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut(A) -> Effect<B, E, R> + Send + 'static,
{
  match it.next() {
    None => succeed(acc),
    Some(x) => f(x).flat_map(move |b| {
      acc.push(b);
      traverse_from(it, f, acc)
    }),
  }
}

/// Traverse a vector with an effectful function (sequential bind; stops at the first failure).
pub fn traverse_vec<A, B, E, R, F>(xs: Vec<A>, f: F) -> Effect<Vec<B>, E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut(A) -> Effect<B, E, R> + Send + 'static,
{
  suspend(move || {
    let n = xs.len();
    traverse_from(xs.into_iter(), f, Vec::with_capacity(n))
  })
}

/// Sequence a vector of effects.
pub fn sequence_vec<A, E, R>(xs: Vec<Effect<A, E, R>>) -> Effect<Vec<A>, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  traverse_vec(xs, |eff| eff)
}

/// Traverse an optional value.
pub fn traverse_option<A, B, E, R, F>(opt: Option<A>, f: F) -> Effect<Option<B>, E, R>
where
  A: 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnOnce(A) -> Effect<B, E, R>,
{
  match opt {
    Some(a) => f(a).map(Some),
    None => succeed(None),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::test_support::run;
  use id_effect::fail;

  #[test]
  fn traverse_vec_empty_input() {
    let eff = traverse_vec(Vec::<i32>::new(), succeed::<i32, (), _>);
    assert_eq!(run(eff), Ok(vec![]));
  }

  #[test]
  fn traverse_vec_collects() {
    let xs = vec![1, 2, 3];
    let eff = traverse_vec(xs, |n| succeed::<i32, (), _>(n * 2));
    assert_eq!(run(eff), Ok(vec![2, 4, 6]));
  }

  #[test]
  fn traverse_vec_stops_at_first_failure() {
    let eff = traverse_vec(vec![1, 2, 3], |n| {
      if n == 2 {
        fail::<i32, i32, _>(n)
      } else {
        succeed(n)
      }
    });
    assert_eq!(run(eff), Err(2));
  }

  #[test]
  fn traverse_vec_handles_long_inputs() {
    let xs: Vec<u32> = (0..10_000).collect();
    let eff = traverse_vec(xs, |n| succeed::<u32, (), _>(n + 1));
    assert_eq!(run(eff).map(|v| v.len()), Ok(10_000));
  }

  #[test]
  fn sequence_vec_empty() {
    let xs: Vec<Effect<i32, ()>> = vec![];
    assert_eq!(run(sequence_vec(xs)), Ok(vec![]));
  }

  #[test]
  fn sequence_vec_runs_all() {
    let xs: Vec<Effect<i32, ()>> = vec![succeed(1), succeed(2)];
    assert_eq!(run(sequence_vec(xs)), Ok(vec![1, 2]));
  }

  #[test]
  fn traverse_option_some() {
    let eff = traverse_option(Some(4i32), |n| succeed::<i32, (), _>(n + 1));
    assert_eq!(run(eff), Ok(Some(5)));
  }

  #[test]
  fn traverse_option_none() {
    let eff = traverse_option(None::<i32>, succeed::<i32, (), _>);
    assert_eq!(run(eff), Ok(None));
  }
}
