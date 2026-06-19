//! **Alternative** — a monoid plus `alt` for choice.

use super::applicative::Applicative;

/// Applicative with an empty element and choice (`alt`).
pub trait Alternative: Applicative {
  /// Empty value.
  fn empty() -> Self;
  /// Prefer `self`, otherwise `other`.
  fn alt(self, other: Self) -> Self;
}

/// [`Option`] alternative helpers.
pub mod option {
  /// Empty option.
  pub fn empty<A>() -> Option<A> {
    None
  }
  /// First `Some` wins.
  pub fn alt<A>(a: Option<A>, b: Option<A>) -> Option<A> {
    a.or(b)
  }
}

/// [`Vec`] alternative helpers (concatenation).
pub mod vec {
  /// Empty vector.
  pub fn empty<A>() -> Vec<A> {
    Vec::new()
  }
  /// Concatenate vectors.
  pub fn alt<A>(mut a: Vec<A>, b: Vec<A>) -> Vec<A> {
    a.extend(b);
    a
  }
}

#[cfg(test)]
mod tests {
  use super::option::{alt, empty};

  #[test]
  fn empty_is_none() {
    assert_eq!(empty::<i32>(), None);
  }

  #[test]
  fn alt_prefers_first_some() {
    assert_eq!(alt(Some(1), Some(2)), Some(1));
  }

  #[test]
  fn alt_falls_back() {
    assert_eq!(alt(None, Some(2)), Some(2));
  }

  mod vec_alt {
    use super::super::vec::{alt, empty};

    #[test]
    fn empty_vec_is_empty() {
      assert!(empty::<i32>().is_empty());
    }

    #[test]
    fn alt_concatenates_vectors() {
      assert_eq!(alt(vec![1], vec![2, 3]), vec![1, 2, 3]);
    }
  }
}
