//! [`Traversal`] — focus on zero or many `A` values inside `S`.

use crate::lens::Lens;
use std::sync::Arc;

/// Optic that may touch many inner values (e.g. every element of a vector).
#[derive(Clone)]
pub struct Traversal<S, A> {
  modify: Arc<dyn Fn(S, Box<dyn FnMut(A) -> A>) -> S + Send + Sync>,
  fold: Arc<dyn Fn(&S, &mut dyn FnMut(A)) + Send + Sync>,
}

impl<S: 'static, A> Traversal<S, A> {
  /// Build a traversal from modify and fold callbacks.
  pub fn new(
    modify: impl Fn(S, Box<dyn FnMut(A) -> A>) -> S + Send + Sync + 'static,
    fold: impl Fn(&S, &mut dyn FnMut(A)) + Send + Sync + 'static,
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
    self.fold_each(source, |a| out.push(a));
    out
  }

  /// Visit every focused value.
  pub fn fold_each(&self, source: &S, mut visit: impl FnMut(A))
  where
    A: Clone,
  {
    (self.fold)(source, &mut visit);
  }
}

/// Traverse every element of a `Vec<A>`.
pub fn vector_each<A: Clone + 'static>() -> Traversal<Vec<A>, A> {
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
pub fn im_vector_each<A: Clone + 'static>() -> Traversal<im::Vector<A>, A> {
  Traversal::new(
    |vec: im::Vector<A>, mut f| vec.iter().cloned().map(f).collect(),
    |vec, visit| {
      for item in vec.iter() {
        visit(item.clone());
      }
    },
  )
}

/// Traverse every element of a vector field through a lens.
pub fn at_vec<S, A>(lens: Lens<S, Vec<A>>) -> Traversal<S, A>
where
  S: Clone + 'static,
  A: Clone + 'static,
{
  let read = lens.clone();
  let write = lens;
  Traversal::new(
    move |s, mut f| {
      write.modify(s, |mut vec| {
        for item in &mut vec {
          *item = f(item.clone());
        }
        vec
      })
    },
    move |s, visit| {
      for item in read.get(s) {
        visit(item);
      }
    },
  )
}

/// Traverse zero or one value in an optional field through a lens.
pub fn at_option<S, T>(lens: Lens<S, Option<T>>) -> Traversal<S, T>
where
  S: Clone + 'static,
  T: Clone + 'static,
{
  let read = lens.clone();
  let write = lens;
  Traversal::new(
    move |s, f| write.modify(s, |opt| opt.map(f)),
    move |s, visit| {
      if let Some(value) = read.get(s) {
        visit(value);
      }
    },
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::lens::field;

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

  mod at_vec {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Boxed {
      items: Vec<i32>,
    }

    #[test]
    fn maps_vector_field_elements() {
      let items = field(
        |b: &Boxed| &b.items,
        |mut b, items| {
          b.items = items;
          b
        },
      );
      let traversal = at_vec(items);
      let updated = traversal.over(Boxed { items: vec![1, 2] }, |n| n + 1);
      assert_eq!(updated.items, vec![2, 3]);
    }
  }

  mod at_option {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Profile {
      nickname: Option<String>,
    }

    #[test]
    fn traverses_present_optional_value() {
      let nickname = field(
        |p: &Profile| &p.nickname,
        |mut p, nickname| {
          p.nickname = nickname;
          p
        },
      );
      let traversal = at_option(nickname);
      let updated = traversal.over(
        Profile {
          nickname: Some("ada".into()),
        },
        |n| n.to_uppercase(),
      );
      assert_eq!(updated.nickname, Some("ADA".into()));
    }
  }
}
