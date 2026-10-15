//! Parallel combinators on fibers: `zip_par`, `all` and `for_each_par` under a
//! [`Concurrency`] policy. Filled by `c-zip-par-all`.
//!
//! A failure fails fast and interrupts the siblings; `Bounded(n)` caps the number in flight;
//! `for_each_par` keeps input order; `Sequential` behaves like a fold.
//!
//! Every combinator here is a thin layer over [`fork_all`], the one choke point for parallel
//! work (ADR 0011 "Parallel combinators"), so they share its semantics:
//!
//! - children are child fibers of the current fiber and inherit its `FiberRef`s;
//! - the first failure interrupts the in-flight siblings, starts no further ones, and waits
//!   until every started child has completed (their finalizers have run) before failing;
//! - the result's cause is that first failure, with any *genuine* sibling failure (`Fail` or
//!   `Die`) combined in as [`Cause::Both`](crate::Cause::Both); the interrupts of siblings
//!   that were stopped because of the first failure are dropped (Effect.ts `Effect.all`);
//! - interrupting the caller interrupts every child.
//!
//! `Concurrency::Bounded(n)` is a purely local window. Only `Concurrency::Inherit` will await
//! Fabric admission (`c-fabric-scheduler`, at `fork_all`'s single admission point); until then
//! it behaves as `Unbounded`.
//!
//! ```
//! use std::time::Duration;
//! use id_effect::Exit;
//! use id_effect::next::{Concurrency, Effect, all, for_each_par, sleep, succeed, zip_par};
//! use id_effect::runtime::Never;
//!
//! let pair: Effect<(u8, &str), Never> = zip_par(
//!   sleep(Duration::from_millis(5)).then(succeed(1)),
//!   succeed("two"),
//! );
//! assert!(matches!(pair.block_on(), Exit::Success((1, "two"))));
//!
//! let squares = for_each_par(1..=4_u32, |n| succeed::<_, Never, _>(n * n), Concurrency::Bounded(2));
//! assert!(matches!(squares.block_on(), Exit::Success(v) if v == [1, 4, 9, 16]));
//!
//! let effs: Vec<Effect<u32, Never>> = vec![succeed(1), succeed(2)];
//! assert!(matches!(all(effs, Concurrency::Sequential).block_on(), Exit::Success(v) if v == [1, 2]));
//! ```

use crate::next::concurrency::{Concurrency, fork_all};
use crate::next::kernel::Effect;

/// One side of a [`zip_with_par`]: both sides go through one homogeneous [`fork_all`].
enum Side<A, B> {
  Left(A),
  Right(B),
}

/// Runs `left` and `right` concurrently (as two child fibers) and pairs their values.
///
/// The first failure interrupts the other side; see the [module docs](self) for the cause.
pub fn zip_par<A, B, E, R>(left: Effect<A, E, R>, right: Effect<B, E, R>) -> Effect<(A, B), E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  zip_with_par(left, right, |a, b| (a, b))
}

/// Runs `left` and `right` concurrently and combines their values with `f`.
///
/// The first failure interrupts the other side; see the [module docs](self) for the cause.
pub fn zip_with_par<A, B, C, E, R>(
  left: Effect<A, E, R>,
  right: Effect<B, E, R>,
  f: impl FnOnce(A, B) -> C + Send + 'static,
) -> Effect<C, E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  C: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  fork_all(
    vec![left.map(Side::Left), right.map(Side::Right)],
    Concurrency::Unbounded,
  )
  .map(move |sides| {
    let mut sides = sides.into_iter();
    match (sides.next(), sides.next()) {
      (Some(Side::Left(a)), Some(Side::Right(b))) => f(a, b),
      _ => unreachable!("next: zip_with_par lost its input order (kernel bug)"),
    }
  })
}

/// Runs `effects` under `concurrency` and collects their values in input order.
///
/// `Sequential` is a left fold on the current fiber (nothing is forked, it stops at the first
/// failure); the other modes fork one child fiber per effect. See the [module docs](self).
pub fn all<A, E, R>(
  effects: impl IntoIterator<Item = Effect<A, E, R>>,
  concurrency: Concurrency,
) -> Effect<Vec<A>, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  fork_all(effects.into_iter().collect(), concurrency)
}

/// Maps every item of `items` to an effect with `f`, runs them under `concurrency`, and
/// collects the values in input order (not completion order).
///
/// `f` is called for every item up front, when the combinator is built; the effects themselves
/// run only when the result runs. See [`all`].
pub fn for_each_par<T, A, E, R>(
  items: impl IntoIterator<Item = T>,
  f: impl FnMut(T) -> Effect<A, E, R>,
  concurrency: Concurrency,
) -> Effect<Vec<A>, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  fork_all(items.into_iter().map(f).collect(), concurrency)
}
