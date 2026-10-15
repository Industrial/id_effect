//! Transducers — composable reducing-function transforms, applied to streams with
//! [`Stream::transduce_items`](super::Stream::transduce_items).
//!
//! Kernel-independent (no effect or runtime use); `k-retire-v1` moved it here from the 0.6
//! `streaming::transducer`, which re-exports it. `id_effect_optics::transducer` re-exports it
//! too.

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

/// Keep at most `count` items (per transducer instance; the count is shared across clones of
/// the reducer it builds).
pub fn take<A: 'static, Acc: 'static>(count: usize) -> Transducer<A, Acc> {
  use std::sync::atomic::{AtomicUsize, Ordering};
  let remaining = Arc::new(AtomicUsize::new(count));
  Transducer::new(move |mut rf| {
    let remaining = remaining.clone();
    Box::new(move |acc, item| {
      let left = remaining.load(Ordering::Relaxed);
      if left == 0 {
        acc
      } else {
        remaining.store(left - 1, Ordering::Relaxed);
        rf(acc, item)
      }
    })
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

  fn push() -> Reducer<i32, Vec<i32>> {
    Box::new(|mut acc, x| {
      acc.push(x);
      acc
    })
  }

  #[test]
  fn custom_transducer_step() {
    let xf = Transducer::new(|mut rf| Box::new(move |acc, item| rf(acc, item)));
    assert_eq!(xf.transduce([1, 2], push(), vec![]), vec![1, 2]);
  }

  #[test]
  fn take_limits_items() {
    let xf = take::<i32, i32>(2);
    let sum = xf.transduce([1, 2, 3], Box::new(|acc, n| acc + n), 0);
    assert_eq!(sum, 3);
  }

  #[test]
  fn composed_map_and_filter() {
    let xf = map(|n: i32| n + 1).compose(filter(|n: &i32| n % 2 == 0));
    assert_eq!(xf.transduce([1, 2, 3], push(), vec![]), vec![2, 4]);
  }
}
