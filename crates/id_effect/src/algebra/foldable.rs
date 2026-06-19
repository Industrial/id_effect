//! **Foldable** — types that can be folded into a summary value.

/// A type that can be folded from the right.
pub trait Foldable {
  /// Element type being folded.
  type Item;
  /// Fold from the right with `init` and combining function `f`.
  fn fold_right<B>(self, init: B, f: impl FnMut(Self::Item, B) -> B) -> B;
  /// Fold from the left (default via reversed `fold_right`).
  fn fold_left<B>(self, init: B, mut f: impl FnMut(B, Self::Item) -> B) -> B
  where
    Self: Sized,
  {
    self.fold_right(init, |a, b| f(b, a))
  }
}

/// Free function for [`Foldable::fold_right`].
#[inline]
pub fn fold_right<F: Foldable, B>(fa: F, init: B, f: impl FnMut(F::Item, B) -> B) -> B {
  fa.fold_right(init, f)
}

/// [`Option`] fold helpers.
pub mod option {
  /// Fold an `Option` from the right.
  pub fn fold_right<A, B>(opt: Option<A>, init: B, mut f: impl FnMut(A, B) -> B) -> B {
    match opt {
      Some(a) => f(a, init),
      None => init,
    }
  }
}

/// [`Vec`] fold helpers.
pub mod vec {
  /// Fold a vector from the right.
  pub fn fold_right<A, B>(xs: Vec<A>, init: B, mut f: impl FnMut(A, B) -> B) -> B {
    xs.into_iter().rev().fold(init, |acc, x| f(x, acc))
  }
}

/// [`EffectVector`](crate::collections::EffectVector) fold helpers.
pub mod effect_vector {
  use crate::collections::EffectVector;
  /// Fold a persistent vector from the right.
  pub fn fold_right<A: Clone, B>(xs: EffectVector<A>, init: B, mut f: impl FnMut(A, B) -> B) -> B {
    xs.iter().rev().fold(init, |acc, x| f(x.clone(), acc))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod option_fold {
    use super::option::fold_right;

    #[test]
    fn none_returns_init() {
      assert_eq!(fold_right(None::<i32>, 0, |a, b| a + b), 0);
    }

    #[test]
    fn some_applies_fn() {
      assert_eq!(fold_right(Some(3), 0, |a, b| a + b), 3);
    }
  }

  mod vec_fold {
    use super::vec::fold_right;

    #[test]
    fn sums_elements() {
      assert_eq!(fold_right(vec![1, 2, 3], 0, |a, b| a + b), 6);
    }
  }

  mod effect_vector_fold {
    use super::effect_vector::fold_right;
    use crate::collections::EffectVector;

    #[test]
    fn sums_persistent_vector() {
      let v: EffectVector<i32> = [1, 2, 3].into_iter().collect();
      assert_eq!(fold_right(v, 0, |a, b| a + b), 6);
    }
  }

  struct ListFoldable(Vec<i32>);

  impl Foldable for ListFoldable {
    type Item = i32;

    fn fold_right<B>(self, init: B, mut f: impl FnMut(Self::Item, B) -> B) -> B {
      self.0.into_iter().rev().fold(init, |acc, x| f(x, acc))
    }
  }

  #[test]
  fn free_fold_right_delegates_to_trait() {
    assert_eq!(
      super::fold_right(ListFoldable(vec![1, 2]), 0, |a, b| a + b),
      3
    );
  }

  #[test]
  fn fold_left_accumulates_in_order() {
    assert_eq!(
      ListFoldable(vec![1, 2, 3]).fold_left(0, |acc, x| acc + x),
      6
    );
  }
}
