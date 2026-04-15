//! Stream consumers — mirrors Effect.ts-style `Sink` (§24).
//!
//! A [`Sink`] describes how to reduce a [`Stream`] into a value. Execute it with [`Sink::run`].

use std::marker::PhantomData;
use std::sync::Arc;

/// Shared predicate for sinks that need [`Clone`] (e.g. [`Sink::collect_all_while`]).
type SharedPredicate<In> = Arc<dyn Fn(&In) -> bool + Send + Sync>;

use crate::collections::hash_map::EffectHashMap;
use crate::coordination::channel::{Channel, consume_sink_accum_stream};
use crate::coordination::queue::Queue;
use crate::foundation::predicate::Predicate;
use crate::kernel::{Effect, box_future};
use crate::streaming::stream::Stream;
use core::any::Any;

#[inline]
fn interruption_requested<R: 'static>(env: &R) -> bool {
  let any = env as &dyn Any;
  any
    .downcast_ref::<crate::runtime::CancellationToken>()
    .is_some_and(crate::runtime::CancellationToken::is_cancelled)
}

pub(crate) struct FoldState<A, In> {
  pub(crate) init: A,
  pub(crate) f: Arc<dyn Fn(A, In) -> A + Send + Sync>,
}

pub(crate) type BoxSinkDriver<A, In, E, R> =
  Arc<dyn Fn(Stream<In, E, R>) -> Effect<A, E, R> + Send + Sync>;

/// Consumer that reduces a [`Stream<In, E, R>`] to a value of type `A`.
pub struct Sink<A, In, E = (), R = ()>
where
  A: 'static,
  In: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  driver: BoxSinkDriver<A, In, E, R>,
  /// Present only for sinks built with [`Sink::fold_left`] / [`Sink::from_fold`], so
  /// [`Sink::zip`] can merge two folds in one pass.
  pub(crate) fold: Option<Arc<FoldState<A, In>>>,
  _pd: PhantomData<(E, R)>,
}

impl<A, In, E, R> Clone for Sink<A, In, E, R>
where
  A: 'static,
  In: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  fn clone(&self) -> Self {
    Self {
      driver: self.driver.clone(),
      fold: self.fold.clone(),
      _pd: PhantomData,
    }
  }
}

impl<A, In, E, R> Sink<A, In, E, R>
where
  A: 'static,
  In: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Run this sink over `stream`.
  #[inline]
  pub fn run(self, stream: Stream<In, E, R>) -> Effect<A, E, R> {
    (self.driver)(stream)
  }

  pub(crate) fn from_driver(driver: BoxSinkDriver<A, In, E, R>) -> Self {
    Sink {
      driver,
      fold: None,
      _pd: PhantomData,
    }
  }
}

impl<A, In, E, R> Sink<A, In, E, R>
where
  A: Send + Sync + Clone + 'static,
  In: Send + Sync + Clone + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Left fold over elements (after the stream ends, the accumulator is the result).
  pub fn fold_left(init: A, f: impl Fn(A, In) -> A + Send + Sync + 'static) -> Self {
    let f: Arc<dyn Fn(A, In) -> A + Send + Sync> = Arc::new(f);
    let state = Arc::new(FoldState {
      init: init.clone(),
      f: f.clone(),
    });
    let st = Channel::<A, In, (), E, R>::from_fold(init, f).sink_accum_inner();
    Sink {
      driver: Arc::new(move |stream| consume_sink_accum_stream(st.clone(), stream)),
      fold: Some(state),
      _pd: PhantomData,
    }
  }

  /// Alias for [`Self::fold_left`].
  #[inline]
  pub fn from_fold(init: A, f: impl Fn(A, In) -> A + Send + Sync + 'static) -> Self {
    Self::fold_left(init, f)
  }

  /// Combine two [`Self::fold_left`] / [`Self::from_fold`] sinks into one pass over the stream.
  ///
  /// # Panics
  ///
  /// Panics if either sink was not built with [`Self::fold_left`] / [`Self::from_fold`].
  pub fn zip<B>(self, other: Sink<B, In, E, R>) -> Sink<(A, B), In, E, R>
  where
    B: Send + Sync + Clone + 'static,
    In: Clone + Send + 'static,
  {
    let sa = self
      .fold
      .expect("Sink::zip requires fold_left/from_fold sink");
    let sb = other
      .fold
      .expect("Sink::zip requires fold_left/from_fold sink");
    let ia = sa.init.clone();
    let ib = sb.init.clone();
    let fa = sa.f.clone();
    let fb = sb.f.clone();
    let state = Arc::new(FoldState {
      init: (ia, ib),
      f: Arc::new(move |(a, b), x: In| (fa(a, x.clone()), fb(b, x))),
    });
    let st = Channel::<(A, B), In, (), E, R>::from_fold(state.init.clone(), state.f.clone())
      .sink_accum_inner();
    Sink {
      driver: Arc::new(move |stream| consume_sink_accum_stream(st.clone(), stream)),
      fold: Some(state),
      _pd: PhantomData,
    }
  }
}

impl<In, E, R> Sink<Vec<In>, In, E, R>
where
  In: Send + Sync + Clone + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Collect all elements into a vector (in stream order).
  pub fn collect() -> Self {
    Sink::fold_left(Vec::new(), |mut v, x| {
      v.push(x);
      v
    })
  }

  /// Take elements while `pred` holds; stops before the first failing element (that element is
  /// not included in the result).
  pub fn collect_all_while(pred: Predicate<In>) -> Self {
    let pred: SharedPredicate<In> = Arc::from(pred);
    Sink {
      driver: Arc::new(move |mut stream: Stream<In, E, R>| {
        let pred = pred.clone();
        Effect::new_async(move |env: &mut R| {
          box_future(async move {
            let mut out = Vec::new();
            'outer: loop {
              if interruption_requested(env) {
                break;
              }
              match stream.poll_next_chunk(env).await? {
                None => break,
                Some(chunk) => {
                  for x in chunk.into_vec() {
                    if !pred(&x) {
                      break 'outer;
                    }
                    out.push(x);
                  }
                }
              }
            }
            Ok(out)
          })
        })
      }),
      fold: None,
      _pd: PhantomData,
    }
  }

  /// Take elements until `pred` becomes true; the first matching element is **not** included.
  pub fn collect_all_until(pred: Predicate<In>) -> Self {
    let pred: SharedPredicate<In> = Arc::from(pred);
    Sink {
      driver: Arc::new(move |mut stream: Stream<In, E, R>| {
        let pred = pred.clone();
        Effect::new_async(move |env: &mut R| {
          box_future(async move {
            let mut out = Vec::new();
            'outer: loop {
              if interruption_requested(env) {
                break;
              }
              match stream.poll_next_chunk(env).await? {
                None => break,
                Some(chunk) => {
                  for x in chunk.into_vec() {
                    if pred(&x) {
                      break 'outer;
                    }
                    out.push(x);
                  }
                }
              }
            }
            Ok(out)
          })
        })
      }),
      fold: None,
      _pd: PhantomData,
    }
  }
}

impl<In, E, R> Sink<(), In, E, R>
where
  In: Send + Sync + Clone + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Discard every element; result is `()`.
  #[inline]
  pub fn drain() -> Self {
    Sink::fold_left((), |(), _| ())
  }

  /// Enqueue each stream element into `queue` (using `()` as the queue offer environment).
  pub fn to_queue(queue: Queue<In>) -> Self {
    Sink {
      driver: Arc::new(move |mut stream: Stream<In, E, R>| {
        let queue = queue.clone();
        Effect::new_async(move |env: &mut R| {
          box_future(async move {
            loop {
              if interruption_requested(env) {
                break;
              }
              match stream.poll_next_chunk(env).await? {
                None => break,
                Some(chunk) => {
                  for x in chunk.into_vec() {
                    let _ = queue
                      .offer(x)
                      .run(&mut ())
                      .await
                      .expect("Queue::offer is infallible");
                  }
                }
              }
            }
            Ok(())
          })
        })
      }),
      fold: None,
      _pd: PhantomData,
    }
  }
}

impl<K, V, E, R> Sink<EffectHashMap<K, V>, (K, V), E, R>
where
  K: std::hash::Hash + Eq + Clone + Send + Sync + 'static,
  V: Clone + Send + Sync + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Collect stream elements `(K, V)` into an [`EffectHashMap`].
  #[inline]
  pub fn collect_to_map() -> Self {
    use crate::collections::hash_map;
    Sink::fold_left(hash_map::empty(), |m, (k, v)| hash_map::set(&m, k, v))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::collections::hash_map;
  use crate::runtime::run_blocking;
  use std::sync::Arc;

  #[test]
  fn sink_collect_gathers_all_elements() {
    let stream = Stream::from_iterable([1u8, 2, 3]);
    let out = pollster::block_on(Sink::collect().run(stream).run(&mut ()));
    assert_eq!(out, Ok(vec![1, 2, 3]));
  }

  #[test]
  fn sink_collect_via_channel_matches_original() {
    let s1 = Stream::from_iterable([1u8, 2, 3]);
    let s2 = Stream::from_iterable([1u8, 2, 3]);
    let ch = Channel::<Vec<u8>, u8, (), (), ()>::from_fold(
      Vec::new(),
      Arc::new(|mut v: Vec<u8>, x| {
        v.push(x);
        v
      }),
    );
    let from_channel = pollster::block_on(ch.consume_stream(s1).run(&mut ()));
    let from_sink_collect = pollster::block_on(Sink::collect().run(s2).run(&mut ()));
    assert_eq!(from_channel, from_sink_collect);
  }

  #[test]
  fn sink_drain_via_channel_discards_all() {
    let s = Stream::from_iterable([1u8, 2, 3]);
    let ch = Channel::<(), u8, (), (), ()>::from_fold((), Arc::new(|(), _| ()));
    let via_channel = pollster::block_on(ch.consume_stream(s).run(&mut ()));
    assert_eq!(via_channel, Ok(()));
    let stream2 = Stream::from_iterable([1u8, 2, 3]);
    assert_eq!(
      pollster::block_on(Sink::drain().run(stream2).run(&mut ())),
      Ok(())
    );
  }

  #[test]
  fn sink_drain_discards_all() {
    let stream = Stream::from_iterable([1u8, 2, 3]);
    let out = pollster::block_on(Sink::drain().run(stream).run(&mut ()));
    assert_eq!(out, Ok(()));
  }

  #[test]
  fn sink_collect_while_stops_at_predicate() {
    let stream = Stream::from_iterable([1u8, 2, 3, 4u8]);
    let pred: Predicate<u8> = Box::new(|x| *x < 3);
    let out = pollster::block_on(Sink::collect_all_while(pred).run(stream).run(&mut ()));
    assert_eq!(out, Ok(vec![1, 2]));
  }

  #[test]
  fn sink_collect_until_stops_before_matching_element() {
    let stream = Stream::from_iterable([1u8, 2, 3, 4u8]);
    let pred: Predicate<u8> = Box::new(|x| *x == 3);
    let out = pollster::block_on(Sink::collect_all_until(pred).run(stream).run(&mut ()));
    assert_eq!(out, Ok(vec![1, 2]));
  }

  #[test]
  fn sink_zip_runs_both_sinks() {
    let s1 = Sink::fold_left(0i32, |a, x: i32| a + x);
    let s2 = Sink::fold_left(0usize, |n, _: i32| n + 1);
    let z = s1.zip(s2);
    let stream = Stream::from_iterable([1, 2, 3]);
    let out = pollster::block_on(z.run(stream).run(&mut ()));
    assert_eq!(out, Ok((6, 3)));
  }

  #[test]
  fn sink_collect_to_map_merges_pairs() {
    let stream = Stream::from_iterable([("a", 1i32), ("b", 2), ("a", 10)]);
    let out = pollster::block_on(Sink::collect_to_map().run(stream).run(&mut ()));
    let m = out.expect("sink");
    assert_eq!(hash_map::get(&m, "a"), Some(&10));
    assert_eq!(hash_map::get(&m, "b"), Some(&2));
  }

  #[test]
  fn sink_to_queue_offers_each_element() {
    let q = run_blocking(Queue::unbounded(), ()).expect("queue");
    let q2 = q.clone();
    let stream = Stream::from_iterable([7u16, 8, 9]);
    pollster::block_on(Sink::to_queue(q).run(stream).run(&mut ())).expect("sink run");
    let a = pollster::block_on(q2.take().run(&mut ())).expect("take");
    let b = pollster::block_on(q2.take().run(&mut ())).expect("take");
    let c = pollster::block_on(q2.take().run(&mut ())).expect("take");
    assert_eq!((a, b, c), (7, 8, 9));
  }
}
