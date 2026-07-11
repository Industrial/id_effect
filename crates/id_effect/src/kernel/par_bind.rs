//! Parallel bind combinators for auto-parallel `effect!` codegen.

use core::future::Future;

use crate::failure::union::Or;
use crate::kernel::Effect;

async fn join2<F0, F1>(f0: F0, f1: F1) -> (F0::Output, F1::Output)
where
  F0: Future,
  F1: Future,
{
  futures::future::join(f0, f1).await
}

async fn join3<F0, F1, F2>(f0: F0, f1: F1, f2: F2) -> (F0::Output, F1::Output, F2::Output)
where
  F0: Future,
  F1: Future,
  F2: Future,
{
  futures::future::join3(f0, f1, f2).await
}

async fn join4<F0, F1, F2, F3>(
  f0: F0,
  f1: F1,
  f2: F2,
  f3: F3,
) -> (F0::Output, F1::Output, F2::Output, F3::Output)
where
  F0: Future,
  F1: Future,
  F2: Future,
  F3: Future,
{
  futures::future::join4(f0, f1, f2, f3).await
}

fn merge_pair<A, B, E0, E1>(r0: Result<A, E0>, r1: Result<B, E1>) -> Result<(A, B), Or<E0, E1>> {
  match (r0, r1) {
    (Ok(a), Ok(b)) => Ok((a, b)),
    (Err(e), _) => Err(Or::Left(e)),
    (_, Err(e)) => Err(Or::Right(e)),
  }
}

fn merge_triple<A, B, C, E>(
  r0: Result<A, E>,
  r1: Result<B, E>,
  r2: Result<C, E>,
) -> Result<(A, B, C), E> {
  let a = r0?;
  let b = r1?;
  let c = r2?;
  Ok((a, b, c))
}

fn merge_quad<A, B, C, D, E>(
  r0: Result<A, E>,
  r1: Result<B, E>,
  r2: Result<C, E>,
  r3: Result<D, E>,
) -> Result<(A, B, C, D), E> {
  let a = r0?;
  let b = r1?;
  let c = r2?;
  let d = r3?;
  Ok((a, b, c, d))
}

/// Join two effect binds against cloned environments (parallel `cap_into_bind`).
pub async fn join_binds2<A0, A1, E0, E1, R>(
  e0: Effect<A0, E0, R>,
  e1: Effect<A1, E1, R>,
  r: R,
) -> Result<(A0, A1), Or<E0, E1>>
where
  R: Clone,
  A0: 'static,
  A1: 'static,
  E0: 'static,
  E1: 'static,
  R: 'static,
{
  let r0 = r.clone();
  let r1 = r;
  let (a, b) = join2(
    async move {
      let mut env = r0;
      e0.run(&mut env).await
    },
    async move {
      let mut env = r1;
      e1.run(&mut env).await
    },
  )
  .await;
  merge_pair(a, b)
}

/// Join three effect binds against cloned environments (parallel `cap_into_bind`).
pub async fn join_binds3<A0, A1, A2, E, R>(
  e0: Effect<A0, E, R>,
  e1: Effect<A1, E, R>,
  e2: Effect<A2, E, R>,
  r: R,
) -> Result<(A0, A1, A2), E>
where
  R: Clone,
  A0: 'static,
  A1: 'static,
  A2: 'static,
  E: 'static,
  R: 'static,
{
  let r0 = r.clone();
  let r1 = r.clone();
  let r2 = r;
  let (a, b, c) = join3(
    async move {
      let mut env = r0;
      e0.run(&mut env).await
    },
    async move {
      let mut env = r1;
      e1.run(&mut env).await
    },
    async move {
      let mut env = r2;
      e2.run(&mut env).await
    },
  )
  .await;
  merge_triple(a, b, c)
}

/// Join four effect binds against cloned environments (parallel `cap_into_bind`).
pub async fn join_binds4<A0, A1, A2, A3, E, R>(
  e0: Effect<A0, E, R>,
  e1: Effect<A1, E, R>,
  e2: Effect<A2, E, R>,
  e3: Effect<A3, E, R>,
  r: R,
) -> Result<(A0, A1, A2, A3), E>
where
  R: Clone,
  A0: 'static,
  A1: 'static,
  A2: 'static,
  A3: 'static,
  E: 'static,
  R: 'static,
{
  let r0 = r.clone();
  let r1 = r.clone();
  let r2 = r.clone();
  let r3 = r;
  let (a, b, c, d) = join4(
    async move {
      let mut env = r0;
      e0.run(&mut env).await
    },
    async move {
      let mut env = r1;
      e1.run(&mut env).await
    },
    async move {
      let mut env = r2;
      e2.run(&mut env).await
    },
    async move {
      let mut env = r3;
      e3.run(&mut env).await
    },
  )
  .await;
  merge_quad(a, b, c, d)
}

/// Flatten `Or<E, E>` for parallel bind error propagation when both arms share `E`.
#[inline]
pub fn flatten_or<E>(or: Or<E, E>) -> E {
  match or {
    Or::Left(e) | Or::Right(e) => e,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::kernel::succeed;
  use pollster::FutureExt;

  #[test]
  fn join_binds2_runs_both() {
    let a = succeed::<i32, (), ()>(1);
    let b = succeed::<i32, (), ()>(2);
    let out = join_binds2(a, b, ()).block_on().unwrap();
    assert_eq!(out, (1, 2));
  }

  #[test]
  fn flatten_or_either_side() {
    assert_eq!(flatten_or(Or::Left(7u8)), 7);
    assert_eq!(flatten_or(Or::Right(9u8)), 9);
  }
}
