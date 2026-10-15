//! [`Sink`]: a reusable consumer that reduces a [`Stream`] to a value (Effect.ts `Sink`).

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::ControlFlow;
use std::sync::Arc;

use super::stream::Stream;
use crate::capability::HNil;
use crate::coordination::Queue;
use crate::failure::Never;
use crate::kernel::Effect;

type Driver<A, In, E, R> = Arc<dyn Fn(Stream<In, E, R>) -> Effect<A, E, R> + Send + Sync>;

/// The fold behind a [`Sink::fold_left`] sink, kept so [`Sink::zip`] can merge two folds into one
/// pass and [`FoldChannel::from_sink`](super::FoldChannel::from_sink) can reuse it.
pub(crate) struct FoldState<A, In> {
  pub(crate) init: A,
  pub(crate) step: Arc<dyn Fn(A, In) -> A + Send + Sync>,
}

/// A consumer that reduces a `Stream<In, E, R>` to an `A`. Run it with [`Sink::run`] (or
/// [`Stream::run`]). Cloning shares the driver.
///
/// ```
/// use id_effect::{Sink, Stream};
/// use id_effect::Never;
///
/// let sum = Sink::<u32, u32, Never>::fold_left(0, |acc, x| acc + x);
/// let out = sum.run(Stream::from_iterable([1, 2, 3])).block_on().into_result();
/// assert_eq!(out.ok(), Some(6));
/// ```
pub struct Sink<A, In, E = Never, R = HNil> {
  driver: Driver<A, In, E, R>,
  pub(crate) fold: Option<Arc<FoldState<A, In>>>,
}

impl<A, In, E, R> Clone for Sink<A, In, E, R> {
  fn clone(&self) -> Self {
    Self {
      driver: Arc::clone(&self.driver),
      fold: self.fold.clone(),
    }
  }
}

impl<A, In, E, R> std::fmt::Debug for Sink<A, In, E, R> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Sink")
      .field("fold", &self.fold.is_some())
      .finish_non_exhaustive()
  }
}

impl<A, In, E, R> Sink<A, In, E, R>
where
  A: Send + 'static,
  In: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// A sink from its driver: what to do with the whole stream.
  pub fn new(driver: impl Fn(Stream<In, E, R>) -> Effect<A, E, R> + Send + Sync + 'static) -> Self {
    Self {
      driver: Arc::new(driver),
      fold: None,
    }
  }

  /// Runs this sink over `stream`.
  pub fn run(&self, stream: Stream<In, E, R>) -> Effect<A, E, R> {
    (self.driver)(stream)
  }
}

impl<A, In, E, R> Sink<A, In, E, R>
where
  A: Clone + Send + Sync + 'static,
  In: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Left fold over the elements; the final accumulator is the result.
  pub fn fold_left(init: A, f: impl Fn(A, In) -> A + Send + Sync + 'static) -> Self {
    Self::from_fold_state(FoldState {
      init,
      step: Arc::new(f),
    })
  }

  /// Alias of [`fold_left`](Self::fold_left).
  pub fn from_fold(init: A, f: impl Fn(A, In) -> A + Send + Sync + 'static) -> Self {
    Self::fold_left(init, f)
  }

  fn from_fold_state(state: FoldState<A, In>) -> Self {
    let state = Arc::new(state);
    let st = Arc::clone(&state);
    Self {
      driver: Arc::new(move |stream: Stream<In, E, R>| {
        let step = Arc::clone(&st.step);
        stream.run_fold(st.init.clone(), move |acc, x| step(acc, x))
      }),
      fold: Some(state),
    }
  }

  /// Combines two [`fold_left`](Self::fold_left) sinks into one pass over the stream.
  ///
  /// # Panics
  ///
  /// If either sink was not built with [`fold_left`](Self::fold_left) /
  /// [`from_fold`](Self::from_fold) (or [`collect`](Sink::collect), [`drain`](Sink::drain),
  /// [`collect_to_map`](Sink::collect_to_map), which are folds).
  pub fn zip<B>(self, other: Sink<B, In, E, R>) -> Sink<(A, B), In, E, R>
  where
    B: Clone + Send + Sync + 'static,
    In: Clone,
  {
    let sa = self
      .fold
      .expect("Sink::zip requires fold_left/from_fold sinks");
    let sb = other
      .fold
      .expect("Sink::zip requires fold_left/from_fold sinks");
    let (fa, fb) = (Arc::clone(&sa.step), Arc::clone(&sb.step));
    Sink::from_fold_state(FoldState {
      init: (sa.init.clone(), sb.init.clone()),
      step: Arc::new(move |(a, b), x: In| (fa(a, x.clone()), fb(b, x))),
    })
  }
}

impl<In, E, R> Sink<Vec<In>, In, E, R>
where
  In: Clone + Send + Sync + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Collects every element, in stream order.
  pub fn collect() -> Self {
    Sink::fold_left(Vec::new(), |mut v, x| {
      v.push(x);
      v
    })
  }

  /// Collects elements while `pred` holds; stops before the first element that fails it (that
  /// element is not included, and the stream is not pulled further).
  pub fn collect_all_while(pred: impl Fn(&In) -> bool + Send + Sync + 'static) -> Self {
    let pred = Arc::new(pred);
    Sink::new(move |stream: Stream<In, E, R>| {
      let pred = Arc::clone(&pred);
      stream.fold_chunks(Vec::new(), move |mut out, items| {
        for x in items {
          if !pred(&x) {
            return ControlFlow::Break(out);
          }
          out.push(x);
        }
        ControlFlow::Continue(out)
      })
    })
  }

  /// Collects elements until `pred` becomes true; the first matching element is **not**
  /// included.
  pub fn collect_all_until(pred: impl Fn(&In) -> bool + Send + Sync + 'static) -> Self {
    Self::collect_all_while(move |x| !pred(x))
  }
}

impl<In, E, R> Sink<(), In, E, R>
where
  In: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Discards every element.
  pub fn drain() -> Self {
    Sink::fold_left((), |(), _| ())
  }

  /// Runs `f` for every element, one after the other.
  pub fn for_each(f: impl Fn(In) -> Effect<(), E, R> + Send + Sync + 'static) -> Self {
    let f = Arc::new(f);
    Sink::new(move |stream: Stream<In, E, R>| {
      let f = Arc::clone(&f);
      stream.run_for_each_effect(move |x| f(x))
    })
  }

  /// Offers every element to `queue` (suspending while a bounded queue is full); elements refused
  /// by a dropping or shut-down queue are discarded.
  pub fn to_queue(queue: Queue<In>) -> Self {
    Sink::new(move |stream: Stream<In, E, R>| {
      let queue = queue.clone();
      stream.run_for_each_effect(move |x| queue.offer(x).map(|_| ()))
    })
  }
}

impl<K, V, E, R> Sink<HashMap<K, V>, (K, V), E, R>
where
  K: Hash + Eq + Clone + Send + Sync + 'static,
  V: Clone + Send + Sync + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Collects `(K, V)` elements into a map (later values win).
  pub fn collect_to_map() -> Self {
    Sink::fold_left(HashMap::new(), |mut m, (k, v)| {
      m.insert(k, v);
      m
    })
  }
}
