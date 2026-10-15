//! [`Stream`]: a pull-based, chunked stream on the 0.7 kernel.
//!
//! A stream is an owned *pull*: a boxed `FnOnce(max) -> Effect<Step>` whose step is either the
//! end or one chunk plus the rest of the stream. Operators move their state (counters, buffers,
//! closures) into the rest they return, so nothing is shared between pulls and there is no lock
//! on the pull path (the 0.6 stream kept its state in an `Arc<Mutex<StreamState>>` locked twice
//! per pull). A failed pull ends the stream: there is no rest to pull again (fusing).
//!
//! The pull effect runs on the consumer's fiber under the consumer's environment `R`, so
//! effectful operators such as [`Stream::map_effect`] never clone `R` (it is shared by the
//! runloop), and interruption is the kernel's: a pull is interrupted like any other effect.

use std::collections::VecDeque;
use std::fmt;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::capability::HNil;
use crate::concurrency::{Concurrency, fork_all};
use crate::coordination::{Queue, QueueError};
use crate::failure::Never;
use crate::kernel::{Effect, fail, succeed, suspend, yield_fiber};
use crate::streaming::chunk::Chunk;

/// The chunk size a consumer asks for when it does not choose one ([`Stream::next_chunk`] and
/// the `run_*` consumers).
pub const DEFAULT_CHUNK_SIZE: usize = 64;

/// One pull's outcome: the end (`None`) or a chunk (possibly empty) and the rest of the stream.
pub(crate) type Step<A, E, R> = Option<(Vec<A>, Stream<A, E, R>)>;

type PullFn<A, E, R> = Box<dyn FnOnce(usize) -> Effect<Step<A, E, R>, E, R> + Send>;

/// The take-once failure slot a queue-backed source reports once its queue is drained.
pub(crate) type FailSlot<E> = Arc<Mutex<Option<E>>>;

/// Locks `m`, ignoring poisoning (the guarded data is always left consistent).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A lazy, pull-based stream of `A` that can fail with `E` and needs the capabilities in `R`.
///
/// Nothing runs until a consumer pulls ([`next_chunk`](Self::next_chunk) or a `run_*` method).
/// Each pull yields at most the requested number of items (default [`DEFAULT_CHUNK_SIZE`]), and
/// transforming operators pull one upstream chunk per downstream pull, so memory stays
/// proportional to the chunk size and infinite sources work.
///
/// ```
/// use id_effect::Stream;
/// use id_effect::Never;
///
/// let out = Stream::<u64, Never>::from_iterable(0..)
///   .map(|n| n * 3)
///   .filter(|n: &u64| n % 2 == 0)
///   .take(4)
///   .run_collect()
///   .block_on()
///   .into_result();
/// assert_eq!(out.ok(), Some(vec![0, 6, 12, 18]));
/// ```
pub struct Stream<A, E = Never, R = HNil> {
  repr: Repr<A, E, R>,
}

enum Repr<A, E, R> {
  Done,
  Pull(PullFn<A, E, R>),
}

impl<A, E, R> fmt::Debug for Stream<A, E, R> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self.repr {
      Repr::Done => "Stream::Done",
      Repr::Pull(_) => "Stream::Pull",
    })
  }
}

/// The result of one synchronous [`Stream::pull_stage`] step over an upstream chunk.
pub(crate) enum StageStep<B> {
  /// Emit these items and keep pulling upstream.
  Emit(Vec<B>),
  /// Emit these items and stop: upstream is dropped and never pulled again.
  Last(Vec<B>),
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  // ---------------------------------------------------------------------------------------------
  // Representation
  // ---------------------------------------------------------------------------------------------

  /// A stream with no elements.
  pub fn empty() -> Self {
    Self { repr: Repr::Done }
  }

  /// A stream from its pull: `f(max)` is called on the first pull.
  pub(crate) fn from_step(
    f: impl FnOnce(usize) -> Effect<Step<A, E, R>, E, R> + Send + 'static,
  ) -> Self {
    Self {
      repr: Repr::Pull(Box::new(f)),
    }
  }

  /// Pulls one chunk of at most `max` (at least 1) items. The pull function runs now (it only
  /// builds the effect); callers that are not already running inside an effect wrap this in
  /// `suspend`.
  pub(crate) fn pull_raw(self, max: usize) -> Effect<Step<A, E, R>, E, R> {
    match self.repr {
      Repr::Done => succeed(None),
      Repr::Pull(f) => f(max.max(1)),
    }
  }

  /// Pulls the next non-empty chunk of at most `max` items (`0` counts as `1`) together with the
  /// rest of the stream, or `None` at the end. A failure ends the stream (there is no rest).
  pub fn next_chunk_with_size(self, max: usize) -> Effect<Option<(Chunk<A>, Self)>, E, R> {
    let max = max.max(1);
    suspend(move || self.next_chunk_now(max))
  }

  /// [`next_chunk_with_size`](Self::next_chunk_with_size) with [`DEFAULT_CHUNK_SIZE`].
  pub fn next_chunk(self) -> Effect<Option<(Chunk<A>, Self)>, E, R> {
    self.next_chunk_with_size(DEFAULT_CHUNK_SIZE)
  }

  fn next_chunk_now(self, max: usize) -> Effect<Option<(Chunk<A>, Self)>, E, R> {
    self.pull_raw(max).flat_map(move |step| match step {
      None => succeed(None),
      // Empty chunks are never surfaced: pull again, yielding so a source that keeps producing
      // nothing stays interruptible and fair.
      Some((items, rest)) if items.is_empty() => {
        yield_fiber().flat_map(move |()| rest.next_chunk_now(max))
      }
      Some((items, rest)) => succeed(Some((Chunk::from_vec(items), rest))),
    })
  }

  // ---------------------------------------------------------------------------------------------
  // Sources
  // ---------------------------------------------------------------------------------------------

  /// A one-element stream.
  pub fn succeed(a: A) -> Self {
    Self::from_vec(vec![a])
  }

  /// A stream that fails with `e` on its first pull.
  pub fn fail(e: E) -> Self {
    Self::from_step(move |_| fail(e))
  }

  /// The elements of `chunk`, served in chunks of the requested size.
  pub fn from_chunk(chunk: Chunk<A>) -> Self {
    Self::from_vec(chunk.into_vec())
  }

  /// The elements of `items`, served in chunks of the requested size.
  pub fn from_vec(items: Vec<A>) -> Self {
    Self::buffered(items.into())
  }

  pub(crate) fn buffered(mut buf: VecDeque<A>) -> Self {
    if buf.is_empty() {
      return Self::empty();
    }
    Self::from_step(move |n| {
      let out: Vec<A> = buf.drain(..n.min(buf.len())).collect();
      succeed(Some((out, Self::buffered(buf))))
    })
  }

  /// A stream over any iterator, advanced lazily (at most one chunk of items per pull), so
  /// infinite iterators are fine.
  pub fn from_iterable<I>(iter: I) -> Self
  where
    I: IntoIterator<Item = A>,
    I::IntoIter: Send + 'static,
  {
    Self::from_iter_state(iter.into_iter())
  }

  fn from_iter_state<It: Iterator<Item = A> + Send + 'static>(mut it: It) -> Self {
    Self::from_step(move |n| {
      let batch: Vec<A> = it.by_ref().take(n).collect();
      succeed((!batch.is_empty()).then(|| (batch, Self::from_iter_state(it))))
    })
  }

  /// Expands `init` with the pure generator `f` until it returns `None` (lazily, at most one
  /// chunk of items per pull).
  pub fn unfold<S, F>(init: S, mut f: F) -> Self
  where
    S: Send + 'static,
    F: FnMut(S) -> Option<(A, S)> + Send + 'static,
  {
    let mut state = Some(init);
    Self::from_iter_state(std::iter::from_fn(move || {
      let (a, s) = f(state.take()?)?;
      state = Some(s);
      Some(a)
    }))
  }

  /// Expands `init` with the effectful generator `f` until it returns `None`. Each pull runs `f`
  /// at most `max` times, one after the other.
  pub fn unfold_effect<S, F>(init: S, f: F) -> Self
  where
    S: Send + 'static,
    F: FnMut(S) -> Effect<Option<(A, S)>, E, R> + Send + 'static,
  {
    Self::from_step(move |n| Self::unfold_gather(init, f, n, Vec::new()))
  }

  fn unfold_gather<S, F>(s: S, mut f: F, n: usize, mut out: Vec<A>) -> Effect<Step<A, E, R>, E, R>
  where
    S: Send + 'static,
    F: FnMut(S) -> Effect<Option<(A, S)>, E, R> + Send + 'static,
  {
    f(s).flat_map(move |next| match next {
      None => succeed((!out.is_empty()).then(|| (out, Self::empty()))),
      Some((a, s)) => {
        out.push(a);
        if out.len() >= n {
          succeed(Some((out, Self::unfold_effect(s, f))))
        } else {
          Self::unfold_gather(s, f, n, out)
        }
      }
    })
  }

  /// An incremental source driven by a pull function: each pull runs `f(max)` once; `Ok(None)`
  /// ends the stream. `max` is a hint: a larger batch is split and served over the following
  /// pulls, and an empty batch is simply pulled again.
  ///
  /// ```
  /// use id_effect::{Stream, succeed};
  /// use id_effect::Never;
  ///
  /// let mut next = 0_u64;
  /// let naturals = Stream::<u64, Never>::from_pull(move |n| {
  ///   let batch: Vec<u64> = (next..next + n as u64).collect();
  ///   next += n as u64;
  ///   succeed(Some(batch))
  /// });
  /// let first = naturals.take(3).run_collect().block_on().into_result();
  /// assert_eq!(first.ok(), Some(vec![0, 1, 2]));
  /// ```
  pub fn from_pull<F>(f: F) -> Self
  where
    F: FnMut(usize) -> Effect<Option<Vec<A>>, E, R> + Send + 'static,
  {
    Self::pull_source(f, VecDeque::new())
  }

  fn pull_source<F>(mut f: F, mut buf: VecDeque<A>) -> Self
  where
    F: FnMut(usize) -> Effect<Option<Vec<A>>, E, R> + Send + 'static,
  {
    Self::from_step(move |n| {
      if !buf.is_empty() {
        let out: Vec<A> = buf.drain(..n.min(buf.len())).collect();
        return succeed(Some((out, Self::pull_source(f, buf))));
      }
      f(n).map(move |batch| {
        batch.map(|mut items| {
          let rest: VecDeque<A> = if items.len() > n {
            items.split_off(n).into()
          } else {
            VecDeque::new()
          };
          (items, Self::pull_source(f, rest))
        })
      })
    })
  }

  /// A **single-batch** stream: the first pull runs `effect` to obtain every element, which are
  /// then served in chunks. Use [`from_pull`](Self::from_pull) or
  /// [`unfold_effect`](Self::unfold_effect) for incremental effectful sources.
  pub fn from_effect(effect: Effect<Vec<A>, E, R>) -> Self {
    Self::from_step(move |n| effect.flat_map(move |items| Self::from_vec(items).pull_raw(n)))
  }

  /// The items offered to `queue`, until it is shut down and drained (each pull takes what is
  /// buffered, up to the requested size, suspending while the queue is empty).
  pub fn from_queue(queue: Queue<A>) -> Self {
    queue_source(queue, Arc::new(Mutex::new(None)), VecDeque::new(), |v| v)
  }

  /// Concatenation: every element of `self`, then every element of `second` (incremental, so
  /// either side may be long or unbounded).
  #[must_use]
  pub fn chain(self, second: Self) -> Self {
    if matches!(self.repr, Repr::Done) {
      return second;
    }
    Self::from_step(move |n| {
      self.pull_raw(n).flat_map(move |step| match step {
        None => second.pull_raw(n),
        Some((items, rest)) => succeed(Some((items, rest.chain(second)))),
      })
    })
  }

  /// Alias of [`chain`](Self::chain) (Effect.ts `Stream.concat`).
  #[must_use]
  pub fn concat(self, second: Self) -> Self {
    self.chain(second)
  }

  // ---------------------------------------------------------------------------------------------
  // Stages
  // ---------------------------------------------------------------------------------------------

  /// A pull stage over `self`: every downstream pull fetches **one** upstream chunk (of
  /// `want(&state, max)` items) and feeds it to `step` (`None` once upstream ends, so the step
  /// can flush). After [`StageStep::Last`] or the upstream end, upstream is dropped. Memory is
  /// one upstream chunk plus whatever `state` retains.
  pub(crate) fn pull_stage<B, S, W, F>(self, state: S, want: W, step: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    S: Send + 'static,
    W: Fn(&S, usize) -> usize + Send + 'static,
    F: FnMut(&mut S, Option<Vec<A>>) -> StageStep<B> + Send + 'static,
  {
    Stream::from_step(move |n| {
      let w = want(&state, n).max(1);
      self.pull_raw(w).map(move |res| {
        let (mut state, mut step) = (state, step);
        let (input, rest) = match res {
          Some((items, rest)) => (Some(items), Some(rest)),
          None => (None, None),
        };
        match (step(&mut state, input), rest) {
          (StageStep::Emit(out), Some(rest)) => Some((out, rest.pull_stage(state, want, step))),
          (StageStep::Emit(out) | StageStep::Last(out), _) => {
            (!out.is_empty()).then(|| (out, Stream::empty()))
          }
        }
      })
    })
  }

  /// [`pull_stage`](Self::pull_stage) without explicit state; asks upstream for the requested
  /// size.
  pub(crate) fn stage<B, F>(self, mut step: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(Option<Vec<A>>) -> StageStep<B> + Send + 'static,
  {
    self.pull_stage((), |(), n| n, move |(), input| step(input))
  }

  /// Maps each element with the pure `f`, per upstream chunk, in parallel through the Compute
  /// Fabric when the chunk is large enough (ADR 0008); output order is preserved.
  pub fn map<B, F>(self, f: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    F: Fn(A) -> B + Send + Sync + 'static,
  {
    self.stage(move |input| StageStep::Emit(input.map_or_else(Vec::new, |v| bulk_map(v, &f))))
  }

  /// Maps each element in order with an `FnMut`, on the pulling fiber.
  pub fn map_serial<B, F>(self, mut f: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(A) -> B + Send + 'static,
  {
    self.stage(move |input| {
      StageStep::Emit(input.map_or_else(Vec::new, |v| v.into_iter().map(&mut f).collect()))
    })
  }

  /// Keeps the elements satisfying `p` (Fabric-parallel per chunk, like [`map`](Self::map)).
  pub fn filter<P>(self, p: P) -> Self
  where
    P: Fn(&A) -> bool + Send + Sync + 'static,
  {
    self.stage(move |input| StageStep::Emit(input.map_or_else(Vec::new, |v| bulk_filter(v, &p))))
  }

  /// Keeps the elements satisfying `p`, in order on the pulling fiber.
  pub fn filter_serial<P>(self, mut p: P) -> Self
  where
    P: FnMut(&A) -> bool + Send + 'static,
  {
    self.stage(move |input| {
      StageStep::Emit(input.map_or_else(Vec::new, |v| v.into_iter().filter(|a| p(a)).collect()))
    })
  }

  /// The elements from the start while `p` holds; ends before the first element that fails it
  /// (upstream is not pulled again after that).
  pub fn take_while<P>(self, mut p: P) -> Self
  where
    P: FnMut(&A) -> bool + Send + 'static,
  {
    self.stage(move |input| {
      let Some(items) = input else {
        return StageStep::Emit(Vec::new());
      };
      let mut out = Vec::with_capacity(items.len());
      for item in items {
        if !p(&item) {
          return StageStep::Last(out);
        }
        out.push(item);
      }
      StageStep::Emit(out)
    })
  }

  /// Drops elements from the start while `p` holds, then yields the remainder.
  pub fn drop_while<P>(self, mut p: P) -> Self
  where
    P: FnMut(&A) -> bool + Send + 'static,
  {
    let mut skipping = true;
    self.stage(move |input| {
      let Some(items) = input else {
        return StageStep::Emit(Vec::new());
      };
      if !skipping {
        return StageStep::Emit(items);
      }
      let out: Vec<A> = items.into_iter().skip_while(|a| p(a)).collect();
      if !out.is_empty() {
        skipping = false;
      }
      StageStep::Emit(out)
    })
  }

  /// At most `n` elements from the start. Never asks upstream for more than the remaining count,
  /// and stops pulling upstream once `n` elements have been produced (so it ends infinite
  /// sources).
  pub fn take(self, n: usize) -> Self {
    if n == 0 {
      return Self::empty();
    }
    self.pull_stage(
      n,
      |remaining: &usize, want| want.min(*remaining),
      |remaining, input| {
        let Some(mut items) = input else {
          return StageStep::Emit(Vec::new());
        };
        items.truncate(*remaining);
        *remaining -= items.len();
        if *remaining == 0 {
          StageStep::Last(items)
        } else {
          StageStep::Emit(items)
        }
      },
    )
  }

  /// Groups the elements into vectors of `size` (the last may be shorter; `size == 0` yields an
  /// empty stream). Buffers at most one partial group between pulls.
  pub fn grouped(self, size: usize) -> Stream<Vec<A>, E, R> {
    if size == 0 {
      return Stream::empty();
    }
    let mut cur: Vec<A> = Vec::with_capacity(size);
    self.stage(move |input| {
      let Some(items) = input else {
        let last = std::mem::take(&mut cur);
        return StageStep::Emit(if last.is_empty() {
          Vec::new()
        } else {
          vec![last]
        });
      };
      let mut out: Vec<Vec<A>> = Vec::new();
      for item in items {
        cur.push(item);
        if cur.len() == size {
          out.push(std::mem::replace(&mut cur, Vec::with_capacity(size)));
        }
      }
      StageStep::Emit(out)
    })
  }

  /// Stateful map: emits `f(&mut state, item)` for each element.
  pub fn scan<S, B, F>(self, state: S, mut f: F) -> Stream<B, E, R>
  where
    S: Send + 'static,
    B: Send + 'static,
    F: FnMut(&mut S, A) -> B + Send + 'static,
  {
    self.pull_stage(
      state,
      |_, n| n,
      move |s, input| {
        StageStep::Emit(input.map_or_else(Vec::new, |v| v.into_iter().map(|a| f(s, a)).collect()))
      },
    )
  }

  /// FSM-style scan: `step(state, item)` returns the next state and an optional output; only
  /// the `Some` outputs are emitted.
  pub fn state_scan<S, B, F>(self, init: S, mut step: F) -> Stream<B, E, R>
  where
    S: Send + 'static,
    B: Send + 'static,
    F: FnMut(S, A) -> (S, Option<B>) + Send + 'static,
  {
    let mut state = Some(init);
    self.stage(move |input| {
      let mut out = Vec::new();
      for item in input.unwrap_or_default() {
        let Some(s) = state.take() else { break };
        let (next, emitted) = step(s, item);
        state = Some(next);
        out.extend(emitted);
      }
      StageStep::Emit(out)
    })
  }

  /// Sequential concatenation of the streams `f` builds from each element (Effect.ts
  /// `Stream.flatMap`): every element of `f(a1)`, then every element of `f(a2)`, and so on.
  pub fn flat_map<B, F>(self, f: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(A) -> Stream<B, E, R> + Send + 'static,
  {
    flat_map_go(Some(self), VecDeque::new(), None, f)
  }

  /// Maps each element with an effect, **sequentially** (ADR 0008 Amendment 2): one element's
  /// effect completes before the next one's starts, so output order and side-effect order both
  /// match stream order. A failure ends the stream with that error. `f` runs on the pulling
  /// fiber under its environment (never cloned).
  ///
  /// Use [`map_effect_par`](Self::map_effect_par) for bounded concurrency.
  pub fn map_effect<B, F>(self, f: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(A) -> Effect<B, E, R> + Send + 'static,
  {
    Stream::from_step(move |n| {
      self.pull_raw(n).flat_map(move |step| match step {
        None => succeed(None),
        Some((items, rest)) => {
          let cap = items.len();
          seq_map(items.into_iter(), f, Vec::with_capacity(cap))
            .map(move |(out, f)| Some((out, rest.map_effect(f))))
        }
      })
    })
  }

  /// Maps each element with an effect, with at most `max(n, 1)` effects in flight
  /// (`Concurrency::Bounded(n)` over each upstream chunk, as child fibers of the pulling fiber).
  /// Output order matches stream order; side-effect order does not. The first failure
  /// interrupts the in-flight effects of the chunk and ends the stream with that error.
  pub fn map_effect_par<B, F>(self, n: usize, f: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    F: Fn(A) -> Effect<B, E, R> + Send + Sync + 'static,
  {
    self.map_effect_par_arc(n.max(1), Arc::new(f))
  }

  fn map_effect_par_arc<B, F>(self, limit: usize, f: Arc<F>) -> Stream<B, E, R>
  where
    B: Send + 'static,
    F: Fn(A) -> Effect<B, E, R> + Send + Sync + 'static,
  {
    Stream::from_step(move |n| {
      self.pull_raw(n).flat_map(move |step| match step {
        None => succeed(None),
        Some((items, rest)) => {
          let mode = if limit == 1 || items.len() <= 1 {
            Concurrency::Sequential
          } else {
            Concurrency::Bounded(limit)
          };
          let effects: Vec<Effect<B, E, R>> = items.into_iter().map(|a| f(a)).collect();
          fork_all(effects, mode).map(move |out| Some((out, rest.map_effect_par_arc(limit, f))))
        }
      })
    })
  }

  /// Transforms the typed error of every pull.
  pub fn map_err<E2, F>(self, f: F) -> Stream<A, E2, R>
  where
    E2: Send + 'static,
    F: Fn(E) -> E2 + Send + Sync + 'static,
  {
    self.map_err_arc(Arc::new(f))
  }

  fn map_err_arc<E2, F>(self, f: Arc<F>) -> Stream<A, E2, R>
  where
    E2: Send + 'static,
    F: Fn(E) -> E2 + Send + Sync + 'static,
  {
    match self.repr {
      Repr::Done => Stream::empty(),
      Repr::Pull(pull) => Stream::from_step(move |n| {
        let g = Arc::clone(&f);
        pull(n)
          .map_err(move |e| g(e))
          .map(move |step| step.map(|(items, rest)| (items, rest.map_err_arc(f))))
      }),
    }
  }

  /// Adds the item count of every chunk this stream yields to `counter` (combine with
  /// wall-clock samples for an items-per-second rate). Failed pulls count nothing.
  #[must_use]
  pub fn with_throughput_counter(self, counter: Arc<AtomicU64>) -> Self {
    match self.repr {
      Repr::Done => self,
      Repr::Pull(pull) => Self::from_step(move |n| {
        pull(n).map(move |step| {
          step.map(|(items, rest)| {
            counter.fetch_add(items.len() as u64, Ordering::Relaxed);
            (items, rest.with_throughput_counter(counter))
          })
        })
      }),
    }
  }

  // ---------------------------------------------------------------------------------------------
  // Consumers
  // ---------------------------------------------------------------------------------------------

  /// Folds whole chunks until the end or until `f` breaks; the core loop of every consumer.
  pub(crate) fn fold_chunks<B, F>(self, init: B, f: F) -> Effect<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(B, Vec<A>) -> ControlFlow<B, B> + Send + 'static,
  {
    suspend(move || self.fold_chunks_now(init, f))
  }

  fn fold_chunks_now<B, F>(self, acc: B, mut f: F) -> Effect<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(B, Vec<A>) -> ControlFlow<B, B> + Send + 'static,
  {
    self
      .pull_raw(DEFAULT_CHUNK_SIZE)
      .flat_map(move |step| match step {
        None => succeed(acc),
        Some((items, rest)) => {
          let empty = items.is_empty();
          match f(acc, items) {
            ControlFlow::Break(b) => succeed(b),
            ControlFlow::Continue(b) if empty => {
              yield_fiber().flat_map(move |()| rest.fold_chunks_now(b, f))
            }
            ControlFlow::Continue(b) => rest.fold_chunks_now(b, f),
          }
        }
      })
  }

  /// Drains the stream into a vector.
  pub fn run_collect(self) -> Effect<Vec<A>, E, R> {
    self.fold_chunks(Vec::new(), |mut acc, items| {
      acc.extend(items);
      ControlFlow::Continue(acc)
    })
  }

  /// Left fold with a pure step.
  pub fn run_fold<B, F>(self, init: B, mut f: F) -> Effect<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(B, A) -> B + Send + 'static,
  {
    self.fold_chunks(init, move |acc, items| {
      ControlFlow::Continue(items.into_iter().fold(acc, &mut f))
    })
  }

  /// Left fold where each step is an effect (run one after the other).
  pub fn run_fold_effect<B, F>(self, init: B, f: F) -> Effect<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(B, A) -> Effect<B, E, R> + Send + 'static,
  {
    suspend(move || self.fold_effect_now(init, f))
  }

  fn fold_effect_now<B, F>(self, acc: B, f: F) -> Effect<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(B, A) -> Effect<B, E, R> + Send + 'static,
  {
    self
      .pull_raw(DEFAULT_CHUNK_SIZE)
      .flat_map(move |step| match step {
        None => succeed(acc),
        Some((items, rest)) => {
          let empty = items.is_empty();
          seq_fold(items.into_iter(), acc, f).flat_map(move |(acc, f)| {
            if empty {
              yield_fiber().flat_map(move |()| rest.fold_effect_now(acc, f))
            } else {
              rest.fold_effect_now(acc, f)
            }
          })
        }
      })
  }

  /// Runs `f` for each element, one after the other.
  pub fn run_for_each_effect<F>(self, mut f: F) -> Effect<(), E, R>
  where
    F: FnMut(A) -> Effect<(), E, R> + Send + 'static,
  {
    self.run_fold_effect((), move |(), a| f(a))
  }

  /// Alias of [`run_for_each_effect`](Self::run_for_each_effect).
  pub fn run_for_each<F>(self, f: F) -> Effect<(), E, R>
  where
    F: FnMut(A) -> Effect<(), E, R> + Send + 'static,
  {
    self.run_for_each_effect(f)
  }

  /// Binary reduction over the elements (`None` for an empty stream).
  pub fn run_reduce<F>(self, mut f: F) -> Effect<Option<A>, E, R>
  where
    F: FnMut(A, A) -> A + Send + 'static,
  {
    self.run_fold(None, move |acc, a| {
      Some(match acc {
        Some(cur) => f(cur, a),
        None => a,
      })
    })
  }

  /// Pulls the stream to its end, discarding the elements.
  pub fn run_drain(self) -> Effect<(), E, R> {
    self.fold_chunks((), |(), _| ControlFlow::Continue(()))
  }

  /// Runs `sink` over this stream.
  pub fn run<B: Send + 'static>(self, sink: super::Sink<B, A, E, R>) -> Effect<B, E, R> {
    sink.run(self)
  }
}

impl<E, R> Stream<i64, E, R>
where
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// The half-open range `[start, end_exclusive)`, produced lazily one chunk per pull.
  pub fn range(start: i64, end_exclusive: i64) -> Self {
    Self::from_iterable(start..end_exclusive)
  }
}

impl<A, R> Stream<A, Never, R>
where
  A: Send + 'static,
  R: Send + Sync + 'static,
{
  /// An infallible stream under any error type.
  pub fn widen<E: Send + 'static>(self) -> Stream<A, E, R> {
    self.map_err(|n| match n {})
  }
}

/// A source over `queue`: each pull takes what is buffered (up to the requested size, suspending
/// while the queue is empty) and `unpack`s it; once the queue is shut down and drained the stream
/// fails with the error in `failure`, if one was stored, and otherwise ends.
pub(crate) fn queue_source<T, A, E, R>(
  queue: Queue<T>,
  failure: FailSlot<E>,
  mut buf: VecDeque<A>,
  unpack: fn(Vec<T>) -> Vec<A>,
) -> Stream<A, E, R>
where
  T: Send + 'static,
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Stream::from_step(move |n| {
    if !buf.is_empty() {
      let out: Vec<A> = buf.drain(..n.min(buf.len())).collect();
      return succeed(Some((out, queue_source(queue, failure, buf, unpack))));
    }
    queue
      .take_up_to::<R>(n)
      .map(Some)
      .catch(|_: QueueError| succeed(None))
      .flat_map(move |taken| match taken {
        Some(raw) => {
          let mut items = unpack(raw);
          let rest: VecDeque<A> = if items.len() > n {
            items.split_off(n).into()
          } else {
            VecDeque::new()
          };
          succeed(Some((items, queue_source(queue, failure, rest, unpack))))
        }
        None => match lock(&failure).take() {
          Some(e) => fail(e),
          None => succeed(None),
        },
      })
  })
}

/// Runs `f` over `items` one effect after the other, collecting the outputs; hands `f` back.
fn seq_map<A, B, E, R, F>(
  mut items: std::vec::IntoIter<A>,
  mut f: F,
  mut out: Vec<B>,
) -> Effect<(Vec<B>, F), E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut(A) -> Effect<B, E, R> + Send + 'static,
{
  match items.next() {
    None => succeed((out, f)),
    Some(a) => f(a).flat_map(move |b| {
      out.push(b);
      seq_map(items, f, out)
    }),
  }
}

/// Folds `items` with the effectful `f`, one step after the other; hands `f` back.
fn seq_fold<A, B, E, R, F>(
  mut items: std::vec::IntoIter<A>,
  acc: B,
  mut f: F,
) -> Effect<(B, F), E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut(B, A) -> Effect<B, E, R> + Send + 'static,
{
  match items.next() {
    None => succeed((acc, f)),
    Some(a) => f(acc, a).flat_map(move |acc| seq_fold(items, acc, f)),
  }
}

/// The state machine of [`Stream::flat_map`]: the current inner stream, the outer elements not
/// expanded yet, and the rest of the outer stream.
fn flat_map_go<A, B, E, R, F>(
  outer: Option<Stream<A, E, R>>,
  mut pending: VecDeque<A>,
  inner: Option<Stream<B, E, R>>,
  mut f: F,
) -> Stream<B, E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut(A) -> Stream<B, E, R> + Send + 'static,
{
  Stream::from_step(move |n| {
    if let Some(inner) = inner {
      return inner.pull_raw(n).flat_map(move |step| match step {
        Some((items, rest)) => succeed(Some((items, flat_map_go(outer, pending, Some(rest), f)))),
        None => flat_map_go(outer, pending, None, f).pull_raw(n),
      });
    }
    if let Some(a) = pending.pop_front() {
      // Recursing once is bounded: the `inner` branch above only builds the inner pull.
      let inner = f(a);
      return flat_map_go(outer, pending, Some(inner), f).pull_raw(n);
    }
    match outer {
      None => succeed(None),
      Some(outer) => outer.pull_raw(n).flat_map(move |step| match step {
        None => succeed(None),
        Some((items, rest)) => flat_map_go(Some(rest), items.into(), None, f).pull_raw(n),
      }),
    }
  })
}

/// Pure bulk map: Fabric-parallel (Rayon) when the chunk is large enough, else in order.
fn bulk_map<A: Send, B: Send, F: Fn(A) -> B + Sync>(items: Vec<A>, f: &F) -> Vec<B> {
  #[cfg(feature = "compute")]
  if crate::compute::should_parallelize(items.len()) {
    use rayon::prelude::*;
    return crate::compute::install_parallel(|| items.into_par_iter().map(f).collect());
  }
  items.into_iter().map(f).collect()
}

/// Pure bulk filter: Fabric-parallel (Rayon) when the chunk is large enough, else in order.
fn bulk_filter<A: Send, P: Fn(&A) -> bool + Sync>(items: Vec<A>, p: &P) -> Vec<A> {
  #[cfg(feature = "compute")]
  if crate::compute::should_parallelize(items.len()) {
    use rayon::prelude::*;
    return crate::compute::install_parallel(|| items.into_par_iter().filter(|a| p(a)).collect());
  }
  items.into_iter().filter(|a| p(a)).collect()
}
