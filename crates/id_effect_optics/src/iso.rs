//! [`Iso`] — bidirectional total conversion between two types.

use crate::lens::Lens;
use std::sync::Arc;

/// Bidirectional optic: total mapping in both directions.
#[derive(Clone)]
pub struct Iso<S, A> {
  to: Arc<dyn Fn(S) -> A + Send + Sync>,
  from: Arc<dyn Fn(A) -> S + Send + Sync>,
}

impl<S: 'static, A: 'static> Iso<S, A> {
  /// Build an isomorphism from forward and backward functions.
  pub fn new(
    to: impl Fn(S) -> A + Send + Sync + 'static,
    from: impl Fn(A) -> S + Send + Sync + 'static,
  ) -> Self {
    Self {
      to: Arc::new(to),
      from: Arc::new(from),
    }
  }

  /// Convert `S` into `A`.
  pub fn to(&self, source: S) -> A {
    (self.to)(source)
  }

  /// Convert `A` into `S`.
  pub fn from(&self, value: A) -> S {
    (self.from)(value)
  }

  /// View this isomorphism as a lens.
  pub fn as_lens(&self) -> Lens<S, A>
  where
    S: Clone,
    A: Clone,
  {
    let to = self.to.clone();
    let from = self.from.clone();
    Lens::new(move |s: &S| to(s.clone()), move |_, a| from(a))
  }

  /// Compose with an inner isomorphism.
  pub fn compose<B>(self, inner: Iso<A, B>) -> Iso<S, B>
  where
    A: 'static,
    B: 'static,
  {
    let outer_to = self.to.clone();
    let outer_from = self.from.clone();
    let inner_to = inner.to.clone();
    let inner_from = inner.from.clone();
    Iso::new(
      move |s| inner_to(outer_to(s)),
      move |b| outer_from(inner_from(b)),
    )
  }
}

/// Identity isomorphism.
pub fn identity_iso<S: Clone + 'static>() -> Iso<S, S> {
  Iso::new(|s: S| s.clone(), |s: S| s)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn round_trips_both_ways() {
    let iso = Iso::new(|n: i32| n.to_string(), |s: String| s.parse().unwrap());
    assert_eq!(iso.to(7), "7");
    assert_eq!(iso.from("9".into()), 9);
  }

  #[test]
  fn as_lens_reads_and_writes() {
    let iso = Iso::new(|n: i32| n as u64, |n: u64| n as i32);
    let lens = iso.as_lens();
    assert_eq!(lens.get(&5), 5);
    assert_eq!(lens.set(5, 9), 9);
  }
}
