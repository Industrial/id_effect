//! [`Transducer`] — composable list transformation without intermediate collections.

use std::sync::Arc;

/// Stateful reducer step: `(accumulator, item) -> accumulator`.
pub type Reducer<A, Acc> = Box<dyn FnMut(Acc, A) -> Acc + Send>;

/// A transducer transforms a reducing function.
pub struct Transducer<A, Acc> {
  step: Arc<dyn Fn(Reducer<A, Acc>) -> Reducer<A, Acc> + Send + Sync>,
}

impl<A: 'static, Acc: 'static> Transducer<A, Acc> {
  /// Wrap a step that rewrites an inner reducer.
  pub fn new(step: impl Fn(Reducer<A, Acc>) -> Reducer<A, Acc> + Send + Sync + 'static) -> Self {
    Self {
      step: Arc::new(step),
    }
  }

  /// Compose transducers: `inner` runs first, then `self`.
  pub fn compose(self, inner: Transducer<A, Acc>) -> Transducer<A, Acc> {
    let outer = self.step.clone();
    let inner_step = inner.step.clone();
    Transducer::new(move |rf| outer(inner_step(rf)))
  }

  /// Run the transducer over an iterator, producing a final accumulator.
  pub fn transduce<I>(&self, iter: I, rf: Reducer<A, Acc>, init: Acc) -> Acc
  where
    I: IntoIterator<Item = A>,
    A: Send + 'static,
    Acc: Send + 'static,
  {
    let mut rf = (self.step)(rf);
    let mut acc = init;
    for item in iter {
      acc = rf(acc, item);
    }
    acc
  }
}

/// Map every item before reducing.
pub fn map<A: 'static, Acc: 'static>(
  f: impl Fn(A) -> A + Send + Sync + 'static,
) -> Transducer<A, Acc> {
  let f = Arc::new(f);
  Transducer::new(move |mut rf| {
    let f = f.clone();
    Box::new(move |acc, item| rf(acc, f(item)))
  })
}

/// Keep only items matching `pred`.
pub fn filter<A: 'static, Acc: 'static>(
  pred: impl Fn(&A) -> bool + Send + Sync + 'static,
) -> Transducer<A, Acc> {
  let pred = Arc::new(pred);
  Transducer::new(move |mut rf| {
    let pred = pred.clone();
    Box::new(
      move |acc, item| {
        if pred(&item) { rf(acc, item) } else { acc }
      },
    )
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  mod map {
    use super::*;

    #[test]
    fn doubles_values_before_sum() {
      let xf = map(|n: i32| n * 2);
      let sum = xf.transduce([1, 2, 3], Box::new(|acc, n| acc + n), 0);
      assert_eq!(sum, 12);
    }
  }

  mod filter {
    use super::*;

    #[test]
    fn keeps_even_values_before_sum() {
      let xf = filter(|n: &i32| n % 2 == 0);
      let sum = xf.transduce([1, 2, 3, 4], Box::new(|acc, n| acc + n), 0);
      assert_eq!(sum, 6);
    }
  }

  mod compose {
    use super::*;

    #[test]
    fn chains_map_and_filter() {
      let xf = map(|n: i32| n + 1).compose(filter(|n: &i32| n % 2 == 0));
      let sum = xf.transduce([1, 2, 3], Box::new(|acc, n| acc + n), 0);
      assert_eq!(sum, 6);
    }
  }
}
