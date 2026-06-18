//! [`Prism`] — partial focus into a sum-type variant.

use std::sync::Arc;

/// Partial optic: `S` may contain an `A` variant.
#[derive(Clone)]
pub struct Prism<S, A> {
  preview: Arc<dyn Fn(&S) -> Option<A> + Send + Sync>,
  review: Arc<dyn Fn(A) -> S + Send + Sync>,
}

impl<S: 'static, A: 'static> Prism<S, A> {
  /// Build a prism from preview and review.
  pub fn new(
    preview: impl Fn(&S) -> Option<A> + Send + Sync + 'static,
    review: impl Fn(A) -> S + Send + Sync + 'static,
  ) -> Self {
    Self {
      preview: Arc::new(preview),
      review: Arc::new(review),
    }
  }

  /// Attempt to read the focused variant.
  pub fn preview(&self, source: &S) -> Option<A>
  where
    A: Clone,
  {
    (self.preview)(source).map(|a| a)
  }

  /// Inject an `A` as `S`.
  pub fn review(&self, value: A) -> S {
    (self.review)(value)
  }

  /// Update the focused variant when present.
  pub fn modify(&self, source: S, f: impl FnOnce(A) -> A) -> S
  where
    A: Clone,
  {
    match self.preview(&source) {
      Some(a) => self.review(f(a)),
      None => source,
    }
  }

  /// Compose with an inner prism.
  pub fn compose<B>(self, inner: Prism<A, B>) -> Prism<S, B>
  where
    A: Clone + 'static,
    B: Clone + 'static,
  {
    let outer_preview = self.preview.clone();
    let outer_review = self.review.clone();
    let inner_preview = inner.preview.clone();
    let inner_review = inner.review.clone();
    Prism::new(
      move |s| outer_preview(s).and_then(|a| inner_preview(&a)),
      move |b| outer_review(inner_review(b)),
    )
  }
}

/// Prism for `Option<T>` when `S = Option<T>`.
pub fn some_prism<T: Clone + 'static>() -> Prism<Option<T>, T> {
  Prism::new(|opt| opt.clone(), Option::Some)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Clone, Debug, PartialEq)]
  enum Shape {
    Circle(f64),
    Rect { w: f64, h: f64 },
  }

  fn circle_prism() -> Prism<Shape, f64> {
    Prism::new(
      |s| match s {
        Shape::Circle(r) => Some(*r),
        Shape::Rect { .. } => None,
      },
      Shape::Circle,
    )
  }

  mod preview {
    use super::*;

    #[test]
    fn returns_some_for_matching_variant() {
      assert_eq!(circle_prism().preview(&Shape::Circle(3.0)), Some(3.0));
    }

    #[test]
    fn returns_none_for_other_variant() {
      assert_eq!(
        circle_prism().preview(&Shape::Rect { w: 1.0, h: 2.0 }),
        None
      );
    }
  }

  mod review {
    use super::*;

    #[test]
    fn injects_variant() {
      assert_eq!(circle_prism().review(5.0), Shape::Circle(5.0));
    }
  }

  mod modify {
    use super::*;

    #[test]
    fn updates_matching_variant() {
      let updated = circle_prism().modify(Shape::Circle(2.0), |r| r * 2.0);
      assert_eq!(updated, Shape::Circle(4.0));
    }

    #[test]
    fn leaves_non_matching_variant_unchanged() {
      let rect = Shape::Rect { w: 1.0, h: 2.0 };
      let updated = circle_prism().modify(rect.clone(), |r| r * 2.0);
      assert_eq!(updated, rect);
    }
  }

  mod compose {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    enum Outer {
      Inner(Inner),
      Other,
    }

    #[derive(Clone, Debug, PartialEq)]
    enum Inner {
      Num(i32),
    }

    #[test]
    fn compose_nested_prisms() {
      let outer = Prism::new(
        |s: &Outer| match s {
          Outer::Inner(i) => Some(i.clone()),
          Outer::Other => None,
        },
        Outer::Inner,
      );
      let inner = Prism::new(
        |i: &Inner| match i {
          Inner::Num(n) => Some(*n),
        },
        Inner::Num,
      );
      let composed = outer.compose(inner);
      assert_eq!(composed.preview(&Outer::Inner(Inner::Num(4))), Some(4));
      assert_eq!(composed.review(7), Outer::Inner(Inner::Num(7)));
    }
  }

  mod some_prism {
    use super::*;

    #[test]
    fn previews_some_and_none() {
      let p = some_prism::<i32>();
      assert_eq!(p.preview(&Some(7)), Some(7));
      assert_eq!(p.preview(&None), None);
    }

    #[test]
    fn review_wraps_value_in_some() {
      let p = some_prism::<i32>();
      assert_eq!(p.review(9), Some(9));
    }
  }
}
