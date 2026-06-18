//! Transducer bridge — [`Stream::transduce_items`] applies `id_effect_optics`-compatible transducers.
//!
//! API-compatible with [`id_effect_optics::Transducer`] when that crate is used separately.

use std::sync::Arc;

use crate::streaming::stream::Stream;

/// Stateful reducer step: `(accumulator, item) -> accumulator`.
pub type Reducer<A, Acc> = Box<dyn FnMut(Acc, A) -> Acc + Send>;

/// A transducer transforms a reducing function (same shape as `id_effect_optics::Transducer`).
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

impl<A, E, R> Stream<A, E, R>
where
  A: Send + Clone + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Apply `xf` to each element, emitting transformed items in order.
  #[inline]
  pub fn transduce_items(self, xf: Transducer<A, Vec<A>>) -> Stream<A, E, R> {
    let step = xf.step.clone();
    Stream::new(move |r: &mut R| {
      let step = step.clone();
      Box::pin(async move {
        let mut upstream = self;
        let mut out = Vec::new();
        loop {
          let Some(chunk) = upstream.poll_next_chunk(r).await? else {
            break;
          };
          for item in chunk.into_vec() {
            let mut pushed = (step)(Box::new(|mut acc: Vec<A>, x: A| {
              acc.push(x);
              acc
            }));
            out.extend(pushed(Vec::new(), item));
          }
        }
        Ok(out)
      })
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
  fn composed_map_and_filter() {
    let xf = map(|n: i32| n + 1).compose(filter(|n: &i32| n % 2 == 0));
    let stream = Stream::from_iterable([1, 2, 3]).via_transducer(xf);
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert_eq!(out, vec![2, 4]);
  }
}
