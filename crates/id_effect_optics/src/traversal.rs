//! [`Traversal`] — focus on zero or more `A` values inside `S`.

use std::sync::Arc;

/// Optic that may touch many inner values (e.g. every element of a vector).
#[derive(Clone)]
pub struct Traversal<S, A> {
  modify: Arc<dyn Fn(S, Box<dyn FnMut(A) -> A>) -> S>,
  fold: Arc<dyn Fn(&S, &mut dyn FnMut(A))>,
}

impl<S, A> Traversal<S, A> {
  /// Build a traversal from modify and fold callbacks.
  pub fn new(
    modify: impl Fn(S, Box<dyn FnMut(A) -> A>) -> S + 'static,
    fold: impl Fn(&S, &mut dyn FnMut(A)) + 'static,
  ) -> Self {
    Self {
      modify: Arc::new(modify),
      fold: Arc::new(fold),
    }
  }

  /// Map a function over every focused value.
  pub fn over(&self, source: S, mut f: impl FnMut(A) -> A + 'static) -> S
  where
    A: Clone,
  {
    (self.modify)(source, Box::new(move |a| f(a)))
  }

  /// Collect focused values into a vector.
  pub fn to_vec(&self, source: &S) -> Vec<A>
  where
    A: Clone,
  {
    let mut out = Vec::new();
    (self.fold)(source, &mut |a| out.push(a));
    out
  }
}

/// Traverse every element of a `Vec<A>`.
pub fn vector_each<A: Clone>() -> Traversal<Vec<A>, A> {
  Traversal::new(
    |mut vec: Vec<A>, mut f| {
      for item in &mut vec {
        *item = f(item.clone());
      }
      vec
    },
    |vec, visit| {
      for item in vec {
        visit(item.clone());
      }
    },
  )
}

/// Traverse every value in an [`im::Vector`].
pub fn im_vector_each<A: Clone>() -> Traversal<im::Vector<A>, A> {
  Traversal::new(
    |vec: im::Vector<A>, mut f| vec.iter().cloned().map(|a| f(a)).collect(),
    |vec, visit| {
      for item in vec.iter() {
        visit(item.clone());
      }
    },
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  mod vector_each {
    use super::*;

    #[test]
    fn over_maps_every_element() {
      let traversal = vector_each::<i32>();
      let updated = traversal.over(vec![1, 2, 3], |n| n + 1);
      assert_eq!(updated, vec![2, 3, 4]);
    }

    #[test]
    fn to_vec_collects_elements() {
      let traversal = vector_each::<i32>();
      assert_eq!(traversal.to_vec(&vec![4, 5]), vec![4, 5]);
    }

    #[test]
    fn over_handles_empty_vector() {
      let traversal = vector_each::<i32>();
      assert_eq!(traversal.over(vec![], |n| n + 1), Vec::<i32>::new());
    }
  }

  mod im_vector_each {
    use super::*;

    #[test]
    fn over_maps_persistent_vector() {
      let traversal = im_vector_each::<i32>();
      let input = im::vector![1, 2];
      let updated = traversal.over(input, |n| n * 10);
      assert_eq!(updated, im::vector![10, 20]);
    }
  }
}
