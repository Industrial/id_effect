//! [`Stream::transduce_items`] applies [`Transducer`]s (composable reducing-function
//! transforms) incrementally to 0.6 streams.
//!
//! [`Transducer`] itself is kernel-independent and lives in `next::streaming::transducer` since
//! `k-retire-v1`; it is re-exported here so 0.6 paths keep working.

pub use crate::next::streaming::transducer::{Reducer, Transducer, filter, map, take};
use crate::streaming::stream::{StageStep, Stream};

impl<A, E, R> Stream<A, E, R>
where
  A: Send + Clone + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Apply `xf` to each element, emitting transformed items in order.
  #[inline]
  pub fn transduce_items(self, xf: Transducer<A, Vec<A>>) -> Stream<A, E, R> {
    // Incremental: one upstream chunk per pull, transformed item by item.
    self.stage(move |input| {
      let mut out = Vec::new();
      for item in input.unwrap_or_default() {
        let push: Reducer<A, Vec<A>> = Box::new(|mut acc: Vec<A>, x: A| {
          acc.push(x);
          acc
        });
        out.extend(xf.transduce(std::iter::once(item), push, Vec::new()));
      }
      StageStep::Emit(out)
    })
  }

  /// Alias for [`Self::transduce_items`] — integrates optics-style transducers on streams.
  #[inline]
  pub fn via_transducer(self, xf: Transducer<A, Vec<A>>) -> Stream<A, E, R> {
    self.transduce_items(xf)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn block_on<F: core::future::Future>(fut: F) -> F::Output {
    pollster::block_on(fut)
  }

  #[test]
  fn map_transducer_doubles_values() {
    let xf = map(|n: i32| n * 2);
    let stream = Stream::from_iterable([1, 2, 3]).via_transducer(xf);
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert_eq!(out, vec![2, 4, 6]);
  }

  #[test]
  fn filter_transducer_keeps_evens() {
    let xf = filter(|n: &i32| n % 2 == 0);
    let stream = Stream::from_iterable([1, 2, 3, 4]).transduce_items(xf);
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert_eq!(out, vec![2, 4]);
  }

  #[test]
  fn custom_transducer_step() {
    let xf = Transducer::new(|mut rf| Box::new(move |acc, item| rf(acc, item)));
    let out = xf.transduce(
      [1, 2],
      Box::new(|mut acc: Vec<i32>, x| {
        acc.push(x);
        acc
      }),
      vec![],
    );
    assert_eq!(out, vec![1, 2]);
  }

  #[test]
  fn transducer_on_infinite_source_is_incremental() {
    let out = crate::testing::assert_bounded_memory(
      |s| s.via_transducer(map(|n: u64| n * 10).compose(filter(|n: &u64| n.is_multiple_of(20)))),
      3,
      64,
    );
    assert_eq!(out, vec![0, 20, 40]);
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
    let stream = Stream::from_iterable([1, 2, 3]).via_transducer(xf);
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert_eq!(out, vec![2, 4]);
  }
}
