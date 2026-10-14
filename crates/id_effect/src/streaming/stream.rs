//! Effect.ts-inspired `Stream` API.
//!
//! A [`Stream`] is **pull-based over bounded chunks**: nothing runs until a consumer calls
//! [`Stream::poll_next_chunk`] (directly or via `run_*`), and each pull yields at most one chunk
//! (default 64 items). Sources ([`Stream::from_pull`], [`Stream::from_iterable`],
//! [`Stream::unfold`], [`Stream::unfold_effect`], [`Stream::range`], [`Stream::from_channel`])
//! produce lazily, and transforming operators (`map`, `filter`, `take`, `scan`, `grouped`,
//! `map_effect`, windows, joins, …) pull one upstream chunk per downstream pull. Memory therefore
//! stays proportional to the chunk size (plus any operator-specific state that is documented on
//! the operator), and pipelines over infinite sources emit incrementally; `take(n)` stops pulling
//! its upstream once `n` items have been produced.
//!
//! [`Stream::new`] / [`Stream::from_effect`] are the exception: they resolve a whole `Vec` in one
//! effect run by definition. Use [`Stream::from_pull`] or [`Stream::unfold_effect`] for incremental
//! effectful sources.
//!
//! Wave 7: the **output** side of any [`crate::coordination::channel::Channel`]`<A, …, …, E, R>` is
//! consumed as a stream of `A` with upstream/read failures [`crate::ChannelReadError`] via
//! [`Stream::from_channel`] / [`crate::coordination::channel::Channel::to_stream`].
//! Duplex [`crate::coordination::channel::QueueChannel`]`<A, A, _>` uses [`Stream::from_duplex_queue_channel`] /
//! [`crate::coordination::channel::QueueChannel::to_stream`] with [`QueueError`] (call
//! [`crate::coordination::channel::QueueChannel::shutdown`] to end a drain). A dedicated
//! incremental `StreamState` arm is deferred while `Stream` stays generic over `E` without a
//! `QueueError` error channel.

use crate::collections::sorted_map::EffectSortedMap;
use crate::coordination::pubsub::PubSub;
use crate::coordination::queue::{Queue, QueueError};
use crate::coordination::semaphore::Semaphore;
use crate::observability::metric::Metric;
use crate::resource::scope::Scope;
use crate::runtime::CancellationToken;
use crate::{Chunk, Effect, Or, Predicate};
use core::any::Any;
use core::fmt;
use futures::stream::{self, StreamExt};
use rayon::prelude::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Ordered window-start → aggregation state, for time-keyed stream operators (tumbling / sliding).
pub type TimeBucketMap<A> = EffectSortedMap<Instant, A>;

/// Update the bucket at `window_start` with `f` (`None` if the bucket did not exist yet).
#[inline]
pub fn merge_time_bucket<A: Clone>(
  map: TimeBucketMap<A>,
  window_start: Instant,
  f: impl FnOnce(Option<A>) -> A,
) -> TimeBucketMap<A> {
  crate::collections::sorted_map::modify(map, window_start, |old| Some(f(old)))
}

/// One pull of a [`Stream::from_pull`] source: `(env, requested_chunk_size) -> next items`.
///
/// `Ok(None)` ends the stream; `Ok(Some(vec![]))` is allowed and simply triggers another pull.
pub type PullFn<A, E, R> = Box<
  dyn for<'a> FnMut(&'a mut R, usize) -> crate::kernel::BoxFuture<'a, Result<Option<Vec<A>>, E>>
    + 'static,
>;

/// Result of one synchronous [`Stream::pull_stage`] step over an upstream chunk.
pub(crate) enum StageStep<B> {
  /// Emit these items and keep pulling upstream.
  Emit(Vec<B>),
  /// Emit these items and stop: upstream is dropped and never polled again.
  Last(Vec<B>),
}

#[allow(clippy::type_complexity)]
pub(crate) enum StreamState<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  Pending(Option<Box<dyn FnOnce(&mut R) -> crate::kernel::BoxFuture<'_, Result<Vec<A>, E>>>>),
  /// Incremental source ([`Stream::from_pull`]); `buffered` holds the overflow when a pull
  /// returned more items than the caller's requested chunk size.
  Pull {
    pull: PullFn<A, E, R>,
    buffered: VecDeque<A>,
  },
  Channel {
    queue: Queue<ChannelMessage<A, E>>,
    buffered: VecDeque<A>,
    closed: bool,
  },
  /// Items from a [`Queue<A>`] (e.g. [`PubSub::subscribe`]); optional upstream error in `shared_fail`.
  DirectQueue {
    queue: Queue<A>,
    buffered: VecDeque<A>,
    closed: bool,
    scope: Option<Scope>,
    shared_fail: Arc<Mutex<Option<E>>>,
  },
  Buffered(VecDeque<A>),
  /// [`Stream::chain`]: drain `first` to completion, then drain `second` incrementally.
  Chaining {
    first: Option<Stream<A, E, R>>,
    second: Stream<A, E, R>,
  },
  Exhausted,
}

#[derive(Clone)]
pub(crate) enum ChannelMessage<A, E> {
  Chunk(Chunk<A>),
  End,
  Fail(E),
}

/// How [`stream_from_channel_with_policy`] behaves when the internal queue is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackpressurePolicy {
  /// Block the producer until space is available.
  BoundedBlock,
  /// Drop the newly offered item when full.
  DropNewest,
  /// Evict the oldest queued item to make room.
  DropOldest,
  /// Surface failure to the producer instead of blocking or dropping.
  Fail,
}

/// Concrete action chosen for one enqueue attempt given policy and fill level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackpressureDecision {
  /// Store the item in the queue.
  Enqueue,
  /// Producer should wait and retry.
  Block,
  /// Discard the new item.
  DropNewest,
  /// Remove the oldest item then accept the new one.
  DropOldest,
  /// Propagate full-queue failure to the caller.
  Fail,
}

/// Maps `(policy, queue_len, capacity)` to a [`BackpressureDecision`] (capacity `< 1` treated as `1`).
#[inline]
pub fn backpressure_decision(
  policy: BackpressurePolicy,
  queue_len: usize,
  capacity: usize,
) -> BackpressureDecision {
  let bounded_capacity = capacity.max(1);
  if queue_len < bounded_capacity {
    return BackpressureDecision::Enqueue;
  }
  match policy {
    BackpressurePolicy::BoundedBlock => BackpressureDecision::Block,
    BackpressurePolicy::DropNewest => BackpressureDecision::DropNewest,
    BackpressurePolicy::DropOldest => BackpressureDecision::DropOldest,
    BackpressurePolicy::Fail => BackpressureDecision::Fail,
  }
}

/// Error when [`send_chunk`] cannot enqueue because the channel is full and
/// [`BackpressurePolicy::Fail`] is in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamChannelFull;

impl fmt::Display for StreamChannelFull {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "stream channel is full (backpressure policy Fail)")
  }
}

impl std::error::Error for StreamChannelFull {}

/// Producer handle for a [`Stream`] built with [`stream_from_channel`] / [`stream_from_channel_with_policy`].
#[derive(Clone)]
pub struct StreamSender<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  queue: Queue<ChannelMessage<A, E>>,
  policy: BackpressurePolicy,
}

impl<A, E> StreamSender<A, E>
where
  A: Send + Clone + 'static,
  E: Send + Clone + 'static,
{
  /// Enqueues a terminal failure for consumers; returns `false` if the queue cannot accept it.
  pub fn fail(&self, error: E) -> bool {
    let msg = ChannelMessage::Fail(error);
    loop {
      match crate::runtime::run_blocking(self.queue.offer(msg.clone()), ()) {
        Ok(true) => return true,
        Ok(false) => std::thread::yield_now(),
        Err(()) => return false,
      }
    }
  }
}

/// Pull-based async stream of `A` with error type `E` and environment `R`.
pub struct Stream<A, E = (), R = ()>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  state: Arc<Mutex<StreamState<A, E, R>>>,
  /// When set, [`Stream::poll_next_chunk`] increments this counter by the number of items in each yielded chunk (for throughput / element-rate observability).
  throughput: Option<Metric<u64, ()>>,
}

/// Compatibility facade preserving the v1 public stream type name.
pub type StreamV1<A, E = (), R = ()> = Stream<A, E, R>;

/// `(consumer streams, pump effect)` from [`Stream::broadcast`].
pub type StreamBroadcastFanout<A, E, R> = (Vec<Stream<A, E, R>>, Effect<(), E, R>);

/// Build a pull stream from a [`Queue`] subscription (used by broadcast / replay fanout).
#[inline]
pub(crate) fn stream_from_direct_queue<A, E, R>(
  queue: Queue<A>,
  scope: Option<Scope>,
  shared_fail: Arc<Mutex<Option<E>>>,
  initial_buffered: VecDeque<A>,
) -> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  Stream {
    state: Arc::new(Mutex::new(StreamState::DirectQueue {
      queue,
      buffered: initial_buffered,
      closed: false,
      scope,
      shared_fail,
    })),
    throughput: None,
  }
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// **Single-batch** stream: the first pull runs `f` once to obtain *all* elements as a `Vec`,
  /// buffers them, and serves them in chunks until exhausted.
  ///
  /// The whole batch is materialized before the first chunk is emitted, so this is only suitable
  /// for finite, reasonably small sources. For incremental or unbounded sources use
  /// [`Stream::from_pull`] or [`Stream::unfold_effect`].
  #[inline]
  pub fn new<F>(f: F) -> Self
  where
    F: FnOnce(&mut R) -> crate::kernel::BoxFuture<'_, Result<Vec<A>, E>> + 'static,
  {
    Self {
      state: Arc::new(Mutex::new(StreamState::Pending(Some(Box::new(f))))),
      throughput: None,
    }
  }

  /// Incremental stream driven by a pull function.
  ///
  /// Each downstream pull calls `f(env, chunk_size)` once (more if it returns an empty batch);
  /// `chunk_size` is the number of items the consumer asked for and is a hint — larger batches are
  /// buffered and served over subsequent pulls. Return `Ok(None)` to end the stream. The returned
  /// future must not borrow `f` itself (only `env`), so state that the future needs should be
  /// moved/cloned into it.
  ///
  /// ```
  /// use id_effect::Stream;
  ///
  /// let mut next = 0_u64;
  /// let naturals = Stream::<u64, (), ()>::from_pull(move |_env: &mut (), n| {
  ///   let batch: Vec<u64> = (next..next + n as u64).collect();
  ///   next += n as u64;
  ///   Box::pin(async move { Ok(Some(batch)) })
  /// });
  /// let first = pollster::block_on(naturals.take(3).run_collect().run(&mut ()));
  /// assert_eq!(first, Ok(vec![0, 1, 2]));
  /// ```
  #[inline]
  pub fn from_pull<F>(f: F) -> Self
  where
    F: for<'a> FnMut(&'a mut R, usize) -> crate::kernel::BoxFuture<'a, Result<Option<Vec<A>>, E>>
      + 'static,
  {
    Self {
      state: Arc::new(Mutex::new(StreamState::Pull {
        pull: Box::new(f),
        buffered: VecDeque::new(),
      })),
      throughput: None,
    }
  }

  /// Stream that yields no elements.
  #[inline]
  pub(crate) fn empty() -> Self {
    Self {
      state: Arc::new(Mutex::new(StreamState::Exhausted)),
      throughput: None,
    }
  }

  /// Pull stage over `self`: every downstream pull fetches **one** upstream chunk (of
  /// `want(&state, chunk_size)` items) and feeds it to `step` (`None` once upstream ends, so the
  /// step can flush). After [`StageStep::Last`] or upstream end, upstream is dropped and never
  /// polled again. Memory is one upstream chunk plus whatever `state` retains.
  pub(crate) fn pull_stage<B, S, W, F>(self, state: S, want: W, step: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    S: 'static,
    W: Fn(&S, usize) -> usize + 'static,
    F: FnMut(&mut S, Option<Vec<A>>) -> StageStep<B> + 'static,
  {
    struct Inner<A, E, R, S, W, F>
    where
      A: Send + 'static,
      E: Send + 'static,
      R: 'static,
    {
      upstream: Option<Stream<A, E, R>>,
      state: S,
      want: W,
      step: F,
    }
    let inner = Arc::new(Mutex::new(Inner {
      upstream: Some(self),
      state,
      want,
      step,
    }));
    Stream::from_pull(move |r: &mut R, n| {
      let inner = Arc::clone(&inner);
      Box::pin(async move {
        let next = {
          let mut g = inner.lock().expect("stream stage mutex poisoned");
          let want = (g.want)(&g.state, n).max(1);
          match g.upstream.as_mut() {
            Some(up) => up.poll_next_chunk_with_size(r, want),
            None => return Ok(None),
          }
        };
        let input = next.await?.map(Chunk::into_vec);
        let ended = input.is_none();
        let mut g = inner.lock().expect("stream stage mutex poisoned");
        let g = &mut *g;
        match (g.step)(&mut g.state, input) {
          StageStep::Emit(out) => {
            if ended {
              g.upstream = None;
            }
            Ok(Some(out))
          }
          StageStep::Last(out) => {
            g.upstream = None;
            Ok(Some(out))
          }
        }
      })
    })
  }

  /// [`Stream::pull_stage`] without explicit state; requests the caller's chunk size upstream.
  pub(crate) fn stage<B, F>(self, mut step: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(Option<Vec<A>>) -> StageStep<B> + 'static,
  {
    self.pull_stage((), |_, n| n, move |_, input| step(input))
  }

  /// Attach a counter incremented by the item count of every chunk successfully returned from [`Stream::poll_next_chunk`].
  ///
  /// Combine with wall-clock samples to derive items-per-second. Does not count failed polls.
  #[must_use]
  pub fn with_throughput_metric(mut self, metric: Metric<u64, ()>) -> Self {
    self.throughput = Some(metric);
    self
  }

  /// Pull the next chunk from the stream. `Ok(None)` means the stream is exhausted.
  pub fn poll_next_chunk<'a>(
    &mut self,
    env: &'a mut R,
  ) -> crate::kernel::BoxFuture<'a, Result<Option<Chunk<A>>, E>> {
    self.poll_next_chunk_with_size(env, 64)
  }

  /// [`Stream::poll_next_chunk`] with an explicit maximum chunk size (`0` yields `Ok(None)`
  /// without consuming anything).
  pub(crate) fn poll_next_chunk_with_size<'a>(
    &mut self,
    env: &'a mut R,
    chunk_size: usize,
  ) -> crate::kernel::BoxFuture<'a, Result<Option<Chunk<A>>, E>> {
    let state = self.state.clone();
    let throughput = self.throughput.clone();
    // `guard` is dropped before every `.await` (see Channel recv + Pending arms); clippy's
    // CFG does not always prove that across the `match`.
    #[allow(clippy::await_holding_lock)]
    Box::pin(async move {
      if chunk_size == 0 {
        return Ok(None);
      }

      loop {
        let mut guard = state.lock().expect("stream state mutex poisoned");
        match &mut *guard {
          StreamState::Channel {
            queue,
            buffered,
            closed,
          } => {
            if buffered.is_empty() && !*closed {
              let q = queue.clone();
              drop(guard);
              let message = q.take().run(&mut ()).await;
              let mut guard = state.lock().expect("stream state mutex poisoned");
              match &mut *guard {
                StreamState::Channel {
                  buffered, closed, ..
                } => match message {
                  Ok(ChannelMessage::Chunk(chunk)) => buffered.extend(chunk.into_vec()),
                  Ok(ChannelMessage::End) => *closed = true,
                  Ok(ChannelMessage::Fail(error)) => return Err(error),
                  Err(QueueError::Disconnected) => *closed = true,
                },
                _ => return Ok(None),
              }
              continue;
            }

            if buffered.is_empty() && *closed {
              *guard = StreamState::Exhausted;
              return Ok(None);
            }

            let count = buffered.len().min(chunk_size);
            let mut out = Vec::with_capacity(count);
            for _ in 0..count {
              if let Some(v) = buffered.pop_front() {
                out.push(v);
              }
            }
            let n = out.len() as u64;
            if let Some(m) = &throughput {
              match m.apply(n).run(&mut ()).await {
                Ok(()) => {}
                Err(never) => match never {},
              }
            }
            return Ok(Some(Chunk::from_vec(out)));
          }
          StreamState::DirectQueue {
            queue,
            buffered,
            closed,
            scope,
            shared_fail,
          } => {
            if let Some(e) = shared_fail
              .lock()
              .expect("shared_fail mutex poisoned")
              .take()
            {
              return Err(e);
            }
            if buffered.is_empty() && !*closed {
              let q = queue.clone();
              drop(guard);
              let recv = q.take().run(&mut ()).await;
              let mut guard = state.lock().expect("stream state mutex poisoned");
              match &mut *guard {
                StreamState::DirectQueue {
                  buffered, closed, ..
                } => match recv {
                  Ok(a) => buffered.push_back(a),
                  Err(QueueError::Disconnected) => *closed = true,
                },
                _ => return Ok(None),
              }
              continue;
            }

            if buffered.is_empty() && *closed {
              if let Some(s) = scope.take() {
                let _ = s.close();
              }
              *guard = StreamState::Exhausted;
              return Ok(None);
            }

            let count = buffered.len().min(chunk_size);
            let mut out = Vec::with_capacity(count);
            for _ in 0..count {
              if let Some(v) = buffered.pop_front() {
                out.push(v);
              }
            }
            let n = out.len() as u64;
            if let Some(m) = &throughput {
              match m.apply(n).run(&mut ()).await {
                Ok(()) => {}
                Err(never) => match never {},
              }
            }
            return Ok(Some(Chunk::from_vec(out)));
          }
          StreamState::Chaining { first, second } => {
            if let Some(mut fs) = first.take() {
              drop(guard);
              match fs.poll_next_chunk_with_size(env, chunk_size).await {
                Ok(Some(chunk)) => {
                  let mut g = state.lock().expect("stream state mutex poisoned");
                  if let StreamState::Chaining { first, .. } = &mut *g {
                    *first = Some(fs);
                  }
                  drop(g);
                  let n = chunk.len() as u64;
                  if let Some(m) = &throughput {
                    match m.apply(n).run(&mut ()).await {
                      Ok(()) => {}
                      Err(never) => match never {},
                    }
                  }
                  return Ok(Some(chunk));
                }
                Err(e) => {
                  // Fuse: a failed stream ends (`fs` and `second` are dropped).
                  *state.lock().expect("stream state mutex poisoned") = StreamState::Exhausted;
                  return Err(e);
                }
                Ok(None) => {
                  continue;
                }
              }
            }
            let out = second.poll_next_chunk_with_size(env, chunk_size).await;
            match &out {
              Ok(Some(chunk)) => {
                let n = chunk.len() as u64;
                if let Some(m) = &throughput {
                  match m.apply(n).run(&mut ()).await {
                    Ok(()) => {}
                    Err(never) => match never {},
                  }
                }
              }
              // Fuse on error as well as on end.
              Ok(None) | Err(_) => {
                *guard = StreamState::Exhausted;
              }
            }
            return out;
          }
          StreamState::Buffered(items) => {
            if items.is_empty() {
              *guard = StreamState::Exhausted;
              return Ok(None);
            }
            let count = items.len().min(chunk_size);
            let mut out = Vec::with_capacity(count);
            for _ in 0..count {
              if let Some(v) = items.pop_front() {
                out.push(v);
              }
            }
            if items.is_empty() {
              *guard = StreamState::Exhausted;
            }
            let n = out.len() as u64;
            if let Some(m) = &throughput {
              match m.apply(n).run(&mut ()).await {
                Ok(()) => {}
                Err(never) => match never {},
              }
            }
            return Ok(Some(Chunk::from_vec(out)));
          }
          StreamState::Exhausted => return Ok(None),
          StreamState::Pull { pull, buffered } => {
            let out: Vec<A> = if buffered.is_empty() {
              let next = pull(&mut *env, chunk_size);
              drop(guard);
              match next.await {
                // Fuse: after an error (or the end) the pull fn is dropped and every later
                // poll yields `Ok(None)`.
                Err(e) => {
                  *state.lock().expect("stream state mutex poisoned") = StreamState::Exhausted;
                  return Err(e);
                }
                Ok(None) => {
                  *state.lock().expect("stream state mutex poisoned") = StreamState::Exhausted;
                  return Ok(None);
                }
                // Empty batches are never surfaced to callers: pull again, but honour
                // cancellation and let cooperative executors run in between.
                Ok(Some(items)) if items.is_empty() => {
                  if interruption_requested(&*env) {
                    return Ok(None);
                  }
                  yield_once().await;
                  continue;
                }
                Ok(Some(mut items)) => {
                  if items.len() > chunk_size {
                    let rest = items.split_off(chunk_size);
                    if let StreamState::Pull { buffered, .. } =
                      &mut *state.lock().expect("stream state mutex poisoned")
                    {
                      buffered.extend(rest);
                    }
                  }
                  items
                }
              }
            } else {
              let count = buffered.len().min(chunk_size);
              let out = buffered.drain(..count).collect();
              drop(guard);
              out
            };
            let n = out.len() as u64;
            if let Some(m) = &throughput {
              match m.apply(n).run(&mut ()).await {
                Ok(()) => {}
                Err(never) => match never {},
              }
            }
            return Ok(Some(Chunk::from_vec(out)));
          }
          StreamState::Pending(make) => {
            let make = make.take();
            drop(guard);
            let Some(make) = make else {
              *state.lock().expect("stream state mutex poisoned") = StreamState::Exhausted;
              return Ok(None);
            };

            let items = make(env).await?;
            if items.is_empty() {
              *state.lock().expect("stream state mutex poisoned") = StreamState::Exhausted;
              return Ok(None);
            }

            let mut queue = VecDeque::from(items);
            let count = queue.len().min(chunk_size);
            let mut out = Vec::with_capacity(count);
            for _ in 0..count {
              if let Some(v) = queue.pop_front() {
                out.push(v);
              }
            }
            if queue.is_empty() {
              *state.lock().expect("stream state mutex poisoned") = StreamState::Exhausted;
            } else {
              *state.lock().expect("stream state mutex poisoned") = StreamState::Buffered(queue);
            }
            let n = out.len() as u64;
            if let Some(m) = &throughput {
              match m.apply(n).run(&mut ()).await {
                Ok(()) => {}
                Err(never) => match never {},
              }
            }
            return Ok(Some(Chunk::from_vec(out)));
          }
        }
      }
    })
  }

  /// **Single-batch** stream: the first pull runs `effect` once to obtain *all* elements (same
  /// shape as [`Stream::new`]). For incremental effectful sources use [`Stream::unfold_effect`] or
  /// [`Stream::from_pull`].
  #[inline]
  pub fn from_effect(effect: Effect<Vec<A>, E, R>) -> Self {
    Self::new(move |r| effect.run(r))
  }

  /// Concatenate: yields all chunks from `self`, then all chunks from `second` (incremental; supports
  /// a long or unbounded second stream).
  #[must_use]
  pub fn chain(self, second: Self) -> Self {
    Self {
      state: Arc::new(Mutex::new(StreamState::Chaining {
        first: Some(self),
        second,
      })),
      throughput: None,
    }
  }

  /// Any [`Channel`](crate::coordination::channel::Channel) as an output [`Stream`] (Wave 7).
  ///
  /// Drains via [`Channel::read`](crate::coordination::channel::Channel::read). [`Channel::to_stream`](crate::coordination::channel::Channel::to_stream) forwards to this constructor.
  ///
  /// Lazy: each pull performs exactly one `read`, so every element is forwarded as soon as it is
  /// available (no read-ahead, no accumulation).
  #[inline]
  #[must_use]
  pub fn from_channel<InElem, OutDone>(
    ch: crate::coordination::channel::Channel<A, InElem, OutDone, E, R>,
  ) -> Stream<A, crate::ChannelReadError<E>, R>
  where
    A: Clone,
    InElem: Send + 'static,
    OutDone: 'static,
  {
    let ch = ch.clone();
    Stream::from_pull(move |env: &mut R, _n| {
      let read = ch.read().run(env);
      crate::box_future(async move { read.await.map(|next| next.map(|x| vec![x])) })
    })
  }

  /// Generates elements by repeatedly running `f` on state until it returns `None`.
  ///
  /// Lazy: each pull runs `f` at most `chunk_size` times, so infinite generators are fine.
  #[inline]
  pub fn unfold_effect<S, F>(init: S, f: F) -> Stream<A, E, R>
  where
    S: 'static,
    F: FnMut(S) -> Effect<Option<(A, S)>, E, R> + 'static,
  {
    // `(seed, generator)`; the seed is `None` once the generator returned `None` (or failed).
    let shared = Arc::new(Mutex::new((Some(init), f)));
    Self::from_pull(move |r: &mut R, n| {
      let shared = Arc::clone(&shared);
      Box::pin(async move {
        let mut out = Vec::new();
        while out.len() < n {
          let step = {
            let mut g = shared.lock().expect("unfold_effect mutex poisoned");
            let (seed, f) = &mut *g;
            match seed.take() {
              Some(s) => f(s),
              None => break,
            }
          };
          match step.run(&mut *r).await? {
            Some((a, s2)) => {
              out.push(a);
              shared.lock().expect("unfold_effect mutex poisoned").0 = Some(s2);
            }
            None => break,
          }
        }
        Ok(if out.is_empty() { None } else { Some(out) })
      })
    })
  }

  /// Drains the stream into a single vector (stops early if `R` embeds a cancelled [`CancellationToken`]).
  #[inline]
  pub fn run_collect(self) -> Effect<Vec<A>, E, R> {
    let mut stream = self;
    Effect::new_async(move |r: &mut R| {
      Box::pin(async move {
        let mut out = Vec::new();
        loop {
          if interruption_requested(r) {
            break;
          }
          match stream.poll_next_chunk(r).await? {
            Some(chunk) => out.extend(chunk.into_vec()),
            None => break,
          }
        }
        Ok(out)
      })
    })
  }

  /// Runs `f` for each element (effectful side channel).
  #[inline]
  pub fn run_for_each_effect<F>(self, mut f: F) -> Effect<(), E, R>
  where
    F: FnMut(A) -> Effect<(), E, R> + 'static,
  {
    let mut stream = self;
    Effect::new_async(move |r: &mut R| {
      Box::pin(async move {
        loop {
          if interruption_requested(r) {
            break;
          }
          let Some(chunk) = stream.poll_next_chunk(r).await? else {
            break;
          };
          for item in chunk.into_vec() {
            f(item).run(r).await?;
          }
        }
        Ok(())
      })
    })
  }

  /// Alias of [`Stream::run_for_each_effect`].
  #[inline]
  pub fn run_for_each<F>(self, f: F) -> Effect<(), E, R>
  where
    F: FnMut(A) -> Effect<(), E, R> + 'static,
  {
    self.run_for_each_effect(f)
  }

  /// Left fold over all elements with a pure step function.
  #[inline]
  pub fn run_fold<B, F>(self, init: B, mut f: F) -> Effect<B, E, R>
  where
    B: 'static,
    F: FnMut(B, A) -> B + 'static,
  {
    let mut stream = self;
    Effect::new_async(move |r: &mut R| {
      Box::pin(async move {
        let mut acc = init;
        loop {
          if interruption_requested(r) {
            break;
          }
          let Some(chunk) = stream.poll_next_chunk(r).await? else {
            break;
          };
          for item in chunk.into_vec() {
            acc = f(acc, item);
          }
        }
        Ok(acc)
      })
    })
  }

  /// Left fold where each step is an [`Effect`].
  #[inline]
  pub fn run_fold_effect<B, F>(self, init: B, mut f: F) -> Effect<B, E, R>
  where
    B: 'static,
    F: FnMut(B, A) -> Effect<B, E, R> + 'static,
  {
    let mut stream = self;
    Effect::new_async(move |r: &mut R| {
      Box::pin(async move {
        let mut acc = init;
        loop {
          if interruption_requested(r) {
            break;
          }
          let Some(chunk) = stream.poll_next_chunk(r).await? else {
            break;
          };
          for item in chunk.into_vec() {
            acc = f(acc, item).run(r).await?;
          }
        }
        Ok(acc)
      })
    })
  }

  /// Maps each element using Fabric-aware implicit parallelism (per upstream chunk, incremental).
  #[inline]
  pub fn map<B, F>(self, f: F) -> Stream<B, E, R>
  where
    A: Send,
    B: Send + 'static,
    F: Fn(A) -> B + Send + Sync + 'static,
  {
    self.stage(move |input| {
      let Some(vec) = input else {
        return StageStep::Emit(Vec::new());
      };
      let len = vec.len();
      let mapped: Vec<B> =
        if crate::parallelism::Parallelism::default().should_parallelize_current(len) {
          crate::compute::install_parallel(|| vec.into_par_iter().map(&f).collect())
        } else {
          vec.into_iter().map(&f).collect()
        };
      StageStep::Emit(mapped)
    })
  }

  /// Maps each element sequentially (`FnMut`).
  #[inline]
  pub fn map_serial<B, F>(self, mut f: F) -> Stream<B, E, R>
  where
    B: Send + 'static,
    F: FnMut(A) -> B + 'static,
  {
    self.stage(move |input| {
      StageStep::Emit(input.map_or_else(Vec::new, |v| v.into_iter().map(&mut f).collect()))
    })
  }

  /// Retains elements satisfying `p` (Fabric-aware implicit parallelism, per upstream chunk).
  #[inline]
  pub fn filter(self, p: Predicate<A>) -> Stream<A, E, R>
  where
    A: Send,
  {
    self.stage(move |input| {
      let Some(vec) = input else {
        return StageStep::Emit(Vec::new());
      };
      let len = vec.len();
      let kept: Vec<A> =
        if crate::parallelism::Parallelism::default().should_parallelize_current(len) {
          crate::compute::install_parallel(|| vec.into_par_iter().filter(|item| p(item)).collect())
        } else {
          vec.into_iter().filter(|item| p(item)).collect()
        };
      StageStep::Emit(kept)
    })
  }

  /// Retains elements satisfying `p`, sequentially.
  #[inline]
  pub fn filter_serial(self, p: Predicate<A>) -> Stream<A, E, R> {
    self.stage(move |input| {
      StageStep::Emit(
        input.map_or_else(Vec::new, |v| v.into_iter().filter(|item| p(item)).collect()),
      )
    })
  }

  /// Yields elements from the start of the stream while `p` holds; ends before the first failure
  /// (upstream is not polled again after that).
  #[inline]
  pub fn take_while(self, p: Predicate<A>) -> Stream<A, E, R> {
    self.stage(move |input| {
      let Some(vec) = input else {
        return StageStep::Emit(Vec::new());
      };
      let mut out = Vec::with_capacity(vec.len());
      for item in vec {
        if !p(&item) {
          return StageStep::Last(out);
        }
        out.push(item);
      }
      StageStep::Emit(out)
    })
  }

  /// Drops elements from the start while `p` holds, then yields the remainder.
  #[inline]
  pub fn drop_while(self, p: Predicate<A>) -> Stream<A, E, R> {
    let mut skipping = true;
    self.stage(move |input| {
      let Some(vec) = input else {
        return StageStep::Emit(Vec::new());
      };
      if !skipping {
        return StageStep::Emit(vec);
      }
      let out: Vec<A> = vec.into_iter().skip_while(|item| p(item)).collect();
      if !out.is_empty() {
        skipping = false;
      }
      StageStep::Emit(out)
    })
  }

  /// At most `n` elements from the start of the stream.
  ///
  /// Never requests more than the remaining count from upstream, and stops polling upstream as
  /// soon as `n` elements have been produced (so `take` terminates infinite sources).
  #[inline]
  pub fn take(self, n: usize) -> Stream<A, E, R> {
    if n == 0 {
      return Stream::empty();
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

  /// Chunks into vectors of length `size` (last chunk may be shorter; `size == 0` yields empty output).
  ///
  /// Incremental: buffers at most one partial group between pulls.
  #[inline]
  pub fn grouped(self, size: usize) -> Stream<Vec<A>, E, R> {
    if size == 0 {
      return Stream::empty();
    }
    let mut cur: Vec<A> = Vec::with_capacity(size);
    self.stage(move |input| {
      let Some(vec) = input else {
        let last = core::mem::take(&mut cur);
        return StageStep::Emit(if last.is_empty() {
          Vec::new()
        } else {
          vec![last]
        });
      };
      let mut out: Vec<Vec<A>> = Vec::new();
      for item in vec {
        cur.push(item);
        if cur.len() == size {
          out.push(core::mem::replace(&mut cur, Vec::with_capacity(size)));
        }
      }
      StageStep::Emit(out)
    })
  }

  /// Fan this stream out to `branches` independent consumers via [`PubSub`] (sliding ring of size `n`).
  ///
  /// Returns `(streams, pump)`. Run `pump` concurrently with pulls on the returned streams (same
  /// environment `R` as the pump); the pump forwards upstream chunks into the hub and shuts it down
  /// when the source ends or fails.
  ///
  /// If the upstream fails, the error is stored for branches; the first branch that polls after the
  /// failure [`take`](Option::take)s it from the shared slot (others may end without seeing `Err`).
  #[inline]
  pub fn broadcast(self, n: usize, branches: usize) -> Effect<StreamBroadcastFanout<A, E, R>, E, R>
  where
    A: Clone,
  {
    Effect::new_async(move |_r: &mut R| {
      Box::pin(async move {
        if branches == 0 {
          let upstream = self;
          let pump = Effect::new_async(move |r2: &mut R| {
            Box::pin(async move {
              let mut upstream = upstream;
              while upstream.poll_next_chunk(r2).await?.is_some() {}
              Ok(())
            })
          });
          return Ok((Vec::new(), pump));
        }

        let cap = n.max(1);
        let ps = crate::runtime::run_blocking(PubSub::sliding(cap), ()).expect("pubsub sliding");
        let hub = Scope::make();
        let shared_fail: Arc<Mutex<Option<E>>> = Arc::new(Mutex::new(None));
        let mut outs = Vec::with_capacity(branches);
        for _ in 0..branches {
          let child = hub.fork();
          let q = match crate::runtime::run_async(ps.clone().subscribe(), child.clone()).await {
            Ok(q) => q,
            Err(e) => match e {},
          };
          outs.push(stream_from_direct_queue(
            q,
            Some(child),
            Arc::clone(&shared_fail),
            VecDeque::new(),
          ));
        }

        let upstream = self;
        let ps_pump = ps.clone();
        let shared_pump = Arc::clone(&shared_fail);
        let hub_pump = hub.clone();
        let pump = Effect::new_async(move |r2: &mut R| {
          let hub_pump = hub_pump.clone();
          let ps_pump = ps_pump.clone();
          let shared_pump = Arc::clone(&shared_pump);
          Box::pin(async move {
            let mut upstream = upstream;
            loop {
              match upstream.poll_next_chunk(r2).await {
                Ok(Some(chunk)) => {
                  for a in chunk.into_vec() {
                    let _ = crate::runtime::run_async(ps_pump.publish(a), ()).await;
                    // `Effect` steps often complete in one poll; without yielding, `tokio::join!`
                    // on the current-thread runtime may run the pump to `hub.close()` before
                    // consumer branches are polled.
                    tokio::task::yield_now().await;
                  }
                }
                Ok(None) => {
                  let _ = crate::runtime::run_async(ps_pump.shutdown(), ()).await;
                  let _ = hub_pump.close();
                  break;
                }
                Err(e) => {
                  *shared_pump.lock().expect("shared_fail mutex poisoned") = Some(e);
                  let _ = crate::runtime::run_async(ps_pump.shutdown(), ()).await;
                  let _ = hub_pump.close();
                  break;
                }
              }
            }
            Ok(())
          })
        });

        Ok((outs, pump))
      })
    })
  }

  /// Maps with an effect per element using Fabric admission-bounded concurrency per chunk.
  ///
  /// Processes one upstream chunk at a time (no unbounded prefetch). Output order matches stream order.
  ///
  /// **Cluster dispatch:** wrap each element in an `Effect` that casts/calls a process-group
  /// member (see `id_effect_node::fanout_cast` + `ProcessGroups`) or a pinned remote pid from
  /// [`crate::compute::pick_placement`]. Concurrency stays admission-bounded locally;
  /// cross-node backpressure is the remote mailbox full / `RemoteFault::Timeout` path.
  #[inline]
  pub fn map_effect<B, E2, F>(self, f: F) -> Stream<B, Or<E, E2>, R>
  where
    A: Send,
    B: Send + 'static,
    E2: Send + 'static,
    R: Clone + Send + Sync + 'static,
    F: Fn(A) -> Effect<B, E2, R> + Send + Sync + 'static,
  {
    let permits = crate::compute::current_adaptive_context()
      .admission_budget
      .max(1);
    let f = Arc::new(f);
    // Created on the first pull, so building the stream never runs an effect.
    let sem_slot: Arc<Mutex<Option<Arc<Semaphore>>>> = Arc::new(Mutex::new(None));
    let mut upstream = self;
    Stream::from_pull(move |r: &mut R, n| {
      let f_chunk = Arc::clone(&f);
      let sem_slot = Arc::clone(&sem_slot);
      let r_chunk = r.clone();
      let next = upstream.poll_next_chunk_with_size(r, n);
      Box::pin(async move {
        let Some(chunk) = next.await.map_err(Or::Left)? else {
          return Ok(None);
        };
        let existing = sem_slot.lock().expect("semaphore slot poisoned").clone();
        let sem_chunk = match existing {
          Some(sem) => sem,
          None => {
            let sem = Arc::new(
              crate::runtime::run_async(Semaphore::make(permits), ())
                .await
                .expect("semaphore make"),
            );
            *sem_slot.lock().expect("semaphore slot poisoned") = Some(Arc::clone(&sem));
            sem
          }
        };
        let vec = chunk.into_vec();
        let results: Vec<Result<(usize, B), E2>> =
          stream::iter(vec.into_iter().enumerate().map(move |(idx, item)| {
            let sem = Arc::clone(&sem_chunk);
            let f = Arc::clone(&f_chunk);
            let mut env = r_chunk.clone();
            async move {
              let _permit = crate::runtime::run_async(sem.acquire_owned(), ())
                .await
                .unwrap_or_else(|e| match e {});
              f(item).run(&mut env).await.map(|b| (idx, b))
            }
          }))
          .buffer_unordered(permits)
          .collect()
          .await;
        let mut pairs = Vec::with_capacity(results.len());
        for res in results {
          match res {
            Ok(pair) => pairs.push(pair),
            Err(e) => return Err(Or::Right(e)),
          }
        }
        pairs.sort_by_key(|(i, _)| *i);
        Ok(Some(pairs.into_iter().map(|(_, b)| b).collect()))
      })
    })
  }

  /// Map each element with `f` with at most `n` concurrent effect runs (via [`Semaphore`]).
  ///
  /// Drains the upstream stream first, then runs mappers in parallel with a permit cap of
  /// `max(n, 1)`. Output order matches the original stream order. Each mapper receives a **clone**
  /// of the environment `R`, so `R` must be [`Clone`], [`Send`], and [`Sync`].
  #[cfg(test)]
  #[inline]
  pub(crate) fn map_par_n<B, F>(self, n: usize, f: F) -> Stream<B, E, R>
  where
    A: Send,
    B: Send + 'static,
    E: Send,
    R: Clone + Send + Sync,
    F: Fn(A) -> Effect<B, E, R> + Send + Sync + 'static,
  {
    let f = Arc::new(f);
    Stream::new(move |r: &mut R| {
      let f = Arc::clone(&f);
      let r_env = r.clone();
      Box::pin(async move {
        let permits = n.max(1);
        let sem = Arc::new(
          crate::runtime::run_async(Semaphore::make(permits), ())
            .await
            .expect("semaphore make"),
        );

        let mut upstream = self;
        let mut all_items = Vec::new();
        loop {
          let Some(chunk) = upstream.poll_next_chunk(r).await? else {
            break;
          };
          all_items.extend(chunk.into_vec());
        }

        let len = all_items.len();
        let results: Vec<Result<(usize, B), E>> =
          stream::iter(all_items.into_iter().enumerate().map(move |(idx, item)| {
            let sem = Arc::clone(&sem);
            let f = Arc::clone(&f);
            let mut env = r_env.clone();
            async move {
              let _permit = crate::runtime::run_async(sem.acquire_owned(), ())
                .await
                .unwrap_or_else(|e| match e {});
              f(item).run(&mut env).await.map(|b| (idx, b))
            }
          }))
          .buffer_unordered(permits)
          .collect()
          .await;

        let mut pairs = Vec::with_capacity(len);
        for res in results {
          match res {
            Ok(pair) => pairs.push(pair),
            Err(e) => return Err(e),
          }
        }
        pairs.sort_by_key(|(i, _)| *i);
        let out: Vec<B> = pairs.into_iter().map(|(_, b)| b).collect();
        debug_assert_eq!(out.len(), len);
        Ok(out)
      })
    })
  }

  /// Like [`Self::map_par_n`] but `n` follows the current Compute Fabric admission budget.
  #[cfg(test)]
  #[inline]
  pub(crate) fn map_par_adaptive<B, F>(self, f: F) -> Stream<B, E, R>
  where
    A: Send,
    B: Send + 'static,
    E: Send,
    R: Clone + Send + Sync,
    F: Fn(A) -> Effect<B, E, R> + Send + Sync + 'static,
  {
    let n = crate::compute::current_adaptive_context().admission_budget;
    self.map_par_n(n.max(1), f)
  }

  /// Optional binary reduction over the stream (`None` if empty).
  #[inline]
  pub fn run_reduce<F>(self, mut f: F) -> Effect<Option<A>, E, R>
  where
    F: FnMut(A, A) -> A + 'static,
  {
    let mut stream = self;
    Effect::new_async(move |r: &mut R| {
      Box::pin(async move {
        let mut acc: Option<A> = None;
        loop {
          if interruption_requested(r) {
            break;
          }
          let Some(chunk) = stream.poll_next_chunk(r).await? else {
            break;
          };
          for item in chunk.into_vec() {
            acc = Some(match acc.take() {
              Some(current) => f(current, item),
              None => item,
            });
          }
        }
        Ok(acc)
      })
    })
  }

  /// Stateful map: emits `f(&mut state, item)` for each element.
  #[inline]
  pub fn scan<S, B, F>(self, state: S, mut f: F) -> Stream<B, E, R>
  where
    S: 'static,
    B: Send + 'static,
    F: FnMut(&mut S, A) -> B + 'static,
  {
    self.pull_stage(
      state,
      |_, n| n,
      move |s, input| {
        StageStep::Emit(
          input.map_or_else(Vec::new, |v| v.into_iter().map(|item| f(s, item)).collect()),
        )
      },
    )
  }
}

/// Yield to the executor once: returns `Pending` after waking itself, so any executor (including
/// [`crate::runtime::run_blocking`]'s poll loop) re-polls promptly.
fn yield_once() -> impl core::future::Future<Output = ()> {
  let mut yielded = false;
  core::future::poll_fn(move |cx| {
    if yielded {
      core::task::Poll::Ready(())
    } else {
      yielded = true;
      cx.waker().wake_by_ref();
      core::task::Poll::Pending
    }
  })
}

fn interruption_requested<R: 'static>(env: &R) -> bool {
  let any = env as &dyn Any;
  any
    .downcast_ref::<CancellationToken>()
    .is_some_and(CancellationToken::is_cancelled)
}

impl Stream<i64, (), ()> {
  /// Half-open range `[start, end_exclusive)`, produced lazily one chunk per pull.
  #[inline]
  pub fn range(start: i64, end_exclusive: i64) -> Self {
    Stream::from_iterable(start..end_exclusive)
  }
}

/// Builds a channel-backed stream and sender with explicit [`BackpressurePolicy`].
pub fn stream_from_channel_with_policy<A, E, R>(
  capacity: usize,
  policy: BackpressurePolicy,
) -> (Stream<A, E, R>, StreamSender<A, E>)
where
  A: Send + Clone + 'static,
  E: Send + Clone + 'static,
  R: 'static,
{
  let cap = capacity.max(1);
  let queue_effect = match policy {
    BackpressurePolicy::BoundedBlock | BackpressurePolicy::Fail => Queue::bounded(cap),
    BackpressurePolicy::DropNewest => Queue::dropping(cap),
    BackpressurePolicy::DropOldest => Queue::sliding(cap),
  };
  let queue =
    crate::runtime::run_blocking(queue_effect, ()).expect("stream queue construction must succeed");
  let sender = StreamSender {
    queue: queue.clone(),
    policy,
  };
  let stream = Stream {
    state: Arc::new(Mutex::new(StreamState::Channel {
      queue,
      buffered: VecDeque::new(),
      closed: false,
    })),
    throughput: None,
  };
  (stream, sender)
}

/// [`stream_from_channel_with_policy`] with [`BackpressurePolicy::BoundedBlock`].
pub fn stream_from_channel<A, E, R>(capacity: usize) -> (Stream<A, E, R>, StreamSender<A, E>)
where
  A: Send + Clone + 'static,
  E: Send + Clone + 'static,
  R: 'static,
{
  stream_from_channel_with_policy(capacity, BackpressurePolicy::BoundedBlock)
}

/// Enqueues a chunk for consumers; may block, drop, or fail per the sender’s policy.
pub fn send_chunk<A, E>(
  sender: &StreamSender<A, E>,
  chunk: Chunk<A>,
) -> Effect<(), StreamChannelFull, ()>
where
  A: Send + Clone + 'static,
  E: Send + Clone + 'static,
{
  let queue = sender.queue.clone();
  let policy = sender.policy;
  let msg = ChannelMessage::Chunk(chunk);
  Effect::new(move |_env: &mut ()| match policy {
    BackpressurePolicy::BoundedBlock => loop {
      match crate::runtime::run_blocking(queue.offer(msg.clone()), ()) {
        Ok(true) => return Ok(()),
        Ok(false) => std::thread::yield_now(),
        Err(()) => unreachable!("Queue::offer is infallible"),
      }
    },
    BackpressurePolicy::Fail => match crate::runtime::run_blocking(queue.offer(msg), ()) {
      Ok(true) => Ok(()),
      Ok(false) | Err(()) => Err(StreamChannelFull),
    },
    BackpressurePolicy::DropNewest | BackpressurePolicy::DropOldest => {
      match crate::runtime::run_blocking(queue.offer(msg), ()) {
        Ok(_) => Ok(()),
        Err(()) => unreachable!("Queue::offer is infallible"),
      }
    }
  })
}

/// Signals end-of-stream to consumers (blocks until accepted when using a blocking queue).
pub fn end_stream<A, E>(sender: StreamSender<A, E>) -> Effect<(), (), ()>
where
  A: Send + Clone + 'static,
  E: Send + Clone + 'static,
{
  let queue = sender.queue.clone();
  Effect::new(move |_env: &mut ()| {
    let msg = ChannelMessage::End;
    loop {
      match crate::runtime::run_blocking(queue.offer(msg.clone()), ()) {
        Ok(true) => return Ok(()),
        Ok(false) => std::thread::yield_now(),
        Err(()) => unreachable!("Queue::offer is infallible"),
      }
    }
  })
}

impl<A, E, R> Drop for Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  fn drop(&mut self) {
    if let Ok(mut g) = self.state.lock()
      && let StreamState::DirectQueue { scope, .. } = &mut *g
      && let Some(s) = scope.take()
    {
      let _ = s.close();
    }
  }
}

impl<A, R> Stream<A, QueueError, R>
where
  A: Send + Clone + 'static,
  R: 'static,
{
  /// [`crate::coordination::channel::QueueChannel`] with the same input and output element type (duplex / identity map-in).
  ///
  /// Drains via [`crate::coordination::channel::QueueChannel::read`]; same bootstrap semantics
  /// as [`Stream::from_channel`]. Use [`crate::coordination::channel::QueueChannel::shutdown`]
  /// so the final [`crate::coordination::channel::QueueChannel::read`] returns `None` and collection can complete.
  /// Lazy: one `read` per pull.
  #[inline]
  #[must_use]
  pub fn from_duplex_queue_channel(
    ch: crate::coordination::channel::QueueChannel<A, A, R>,
  ) -> Self {
    let ch = ch.clone();
    Stream::from_pull(move |env: &mut R, _n| {
      let read = ch.read().run(env);
      crate::box_future(async move { read.await.map(|next| next.map(|x| vec![x])) })
    })
  }
}

impl<A: Send + 'static> Stream<A, (), ()> {
  /// Stream over any iterator, pulled lazily (up to one chunk of items per pull).
  ///
  /// The iterator is advanced only as the stream is consumed, so infinite iterators are fine.
  #[inline]
  pub fn from_iterable<I>(iter: I) -> Self
  where
    I: IntoIterator<Item = A> + 'static,
    I::IntoIter: 'static,
  {
    let mut iter = iter.into_iter();
    Stream::from_pull(move |_r: &mut (), n| {
      let batch: Vec<A> = iter.by_ref().take(n).collect();
      Box::pin(async move { Ok((!batch.is_empty()).then_some(batch)) })
    })
  }

  /// Pure [`Stream::unfold_effect`]: expands state with `f` until `None` (lazily, one chunk per
  /// pull).
  #[inline]
  pub fn unfold<S, F>(init: S, mut f: F) -> Self
  where
    S: 'static,
    F: FnMut(S) -> Option<(A, S)> + 'static,
  {
    let mut state = Some(init);
    Stream::from_pull(move |_r: &mut (), n| {
      let mut batch: Vec<A> = Vec::new();
      while batch.len() < n {
        let Some(s) = state.take() else { break };
        let Some((a, s2)) = f(s) else { break };
        batch.push(a);
        state = Some(s2);
      }
      Box::pin(async move { Ok((!batch.is_empty()).then_some(batch)) })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{fail, succeed};
  use core::future::Future;
  use core::task::{Context, Poll, Waker};
  use rstest::rstest;
  use std::sync::Arc;
  use std::task::Wake;
  use std::thread;

  struct ThreadUnpark(thread::Thread);
  impl Wake for ThreadUnpark {
    fn wake(self: Arc<Self>) {
      self.0.unpark();
    }
  }

  fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = Box::pin(fut);
    let waker = Waker::from(Arc::new(ThreadUnpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
      match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => return v,
        Poll::Pending => thread::park(),
      }
    }
  }

  mod time_window_buckets {
    use super::*;
    use crate::collections::sorted_map;
    use std::time::Duration;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct AggState {
      sum: i32,
      cnt: usize,
    }

    #[test]
    fn merge_time_bucket_orders_keys_for_ordered_iteration() {
      let t0 = Instant::now();
      let t1 = t0 + Duration::from_secs(1);
      let t2 = t0 + Duration::from_secs(2);
      let m = sorted_map::empty();
      let m = merge_time_bucket(m, t2, |_| AggState { sum: 2, cnt: 1 });
      let m = merge_time_bucket(m, t0, |_| AggState { sum: 0, cnt: 1 });
      let m = merge_time_bucket(m, t1, |_| AggState { sum: 1, cnt: 1 });
      let keys: Vec<Instant> = m.iter().map(|(k, _)| *k).collect();
      assert_eq!(keys, vec![t0, t1, t2]);
    }

    #[test]
    fn merge_time_bucket_accumulates_same_window() {
      let t0 = Instant::now();
      let m = sorted_map::empty();
      let m = merge_time_bucket(m, t0, |_| AggState { sum: 3, cnt: 1 });
      let m = merge_time_bucket(m, t0, |o| {
        let a = o.expect("prior");
        AggState {
          sum: a.sum + 5,
          cnt: a.cnt + 1,
        }
      });
      assert_eq!(m.get(&t0), Some(&AggState { sum: 8, cnt: 2 }));
    }
  }

  mod constructors {
    use super::*;

    #[test]
    fn from_iterable_with_values_collects_values_in_original_order() {
      let stream = Stream::from_iterable([1, 2, 3]);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![1, 2, 3]));
    }

    #[test]
    fn poll_next_chunk_with_chunk_size_returns_incremental_slices_then_none() {
      let mut stream = Stream::from_iterable([1, 2, 3, 4, 5]);
      let mut env = ();
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 2)),
        Ok(Some(Chunk::from_vec(vec![1, 2])))
      );
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 2)),
        Ok(Some(Chunk::from_vec(vec![3, 4])))
      );
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 2)),
        Ok(Some(Chunk::from_vec(vec![5])))
      );
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 2)),
        Ok(None)
      );
    }

    #[test]
    fn poll_next_chunk_with_zero_chunk_size_returns_none_without_consuming_stream() {
      let mut stream = Stream::from_iterable([1, 2, 3]);
      let mut env = ();
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 0)),
        Ok(None)
      );
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 3)),
        Ok(Some(Chunk::from_vec(vec![1, 2, 3])))
      );
    }

    #[test]
    fn range_with_start_and_end_builds_expected_half_open_interval() {
      let stream = Stream::range(3, 7);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![3, 4, 5, 6]));
    }

    #[test]
    fn unfold_with_generator_stops_when_generator_returns_none() {
      let stream = Stream::unfold(0, |s| if s < 3 { Some((s, s + 1)) } else { None });
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![0, 1, 2]));
    }

    #[test]
    fn stream_from_channel_with_chunks_collects_chunks_in_order() {
      let (stream, sender) = stream_from_channel::<i32, &'static str, ()>(3);
      assert_eq!(
        block_on(send_chunk(&sender, Chunk::from_vec(vec![1, 2])).run(&mut ())),
        Ok(())
      );
      assert_eq!(
        block_on(send_chunk(&sender, Chunk::from_vec(vec![3, 4])).run(&mut ())),
        Ok(())
      );
      assert_eq!(block_on(end_stream(sender).run(&mut ())), Ok(()));

      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![1, 2, 3, 4]));
    }

    #[test]
    fn stream_from_channel_with_producer_failure_propagates_failure() {
      let (stream, sender) = stream_from_channel::<i32, &'static str, ()>(1);
      assert!(sender.fail("producer-failure"));
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Err("producer-failure"));
    }
  }

  mod backpressure {
    use super::*;

    #[rstest]
    #[case::bounded_block(BackpressurePolicy::BoundedBlock)]
    #[case::drop_newest(BackpressurePolicy::DropNewest)]
    #[case::drop_oldest(BackpressurePolicy::DropOldest)]
    #[case::fail(BackpressurePolicy::Fail)]
    fn backpressure_decision_with_non_full_queue_always_enqueues(
      #[case] policy: BackpressurePolicy,
    ) {
      assert_eq!(
        backpressure_decision(policy, 0, 4),
        BackpressureDecision::Enqueue
      );
    }

    #[rstest]
    #[case::bounded_block(BackpressurePolicy::BoundedBlock, BackpressureDecision::Block)]
    #[case::drop_newest(BackpressurePolicy::DropNewest, BackpressureDecision::DropNewest)]
    #[case::drop_oldest(BackpressurePolicy::DropOldest, BackpressureDecision::DropOldest)]
    #[case::fail(BackpressurePolicy::Fail, BackpressureDecision::Fail)]
    fn backpressure_decision_with_full_queue_matches_policy_contract(
      #[case] policy: BackpressurePolicy,
      #[case] expected: BackpressureDecision,
    ) {
      assert_eq!(backpressure_decision(policy, 4, 4), expected);
    }

    #[test]
    fn backpressure_decision_with_zero_capacity_treats_capacity_as_one() {
      assert_eq!(
        backpressure_decision(BackpressurePolicy::BoundedBlock, 0, 0),
        BackpressureDecision::Enqueue
      );
      assert_eq!(
        backpressure_decision(BackpressurePolicy::Fail, 1, 0),
        BackpressureDecision::Fail
      );
    }

    #[test]
    fn stream_from_channel_backpressure_bounded_blocks() {
      let (stream, sender) =
        stream_from_channel_with_policy::<i32, (), ()>(1, BackpressurePolicy::BoundedBlock);
      let sender_clone = sender.clone();
      let producer = thread::spawn(move || {
        assert_eq!(
          block_on(send_chunk(&sender_clone, Chunk::from_vec(vec![1])).run(&mut ())),
          Ok(())
        );
        assert_eq!(
          block_on(send_chunk(&sender_clone, Chunk::from_vec(vec![2])).run(&mut ())),
          Ok(())
        );
        assert_eq!(block_on(end_stream(sender_clone).run(&mut ())), Ok(()));
      });
      let out = block_on(stream.run_collect().run(&mut ()));
      producer.join().expect("producer thread");
      assert_eq!(out, Ok(vec![1, 2]));
    }

    #[test]
    fn stream_from_channel_dropping_discards_newest() {
      let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
      let (stream, sender) =
        stream_from_channel_with_policy::<i32, (), ()>(2, BackpressurePolicy::DropNewest);
      let s2 = sender.clone();
      let producer = thread::spawn(move || {
        assert_eq!(
          block_on(send_chunk(&s2, Chunk::from_vec(vec![1])).run(&mut ())),
          Ok(())
        );
        assert_eq!(
          block_on(send_chunk(&s2, Chunk::from_vec(vec![2])).run(&mut ())),
          Ok(())
        );
        assert_eq!(
          block_on(send_chunk(&s2, Chunk::from_vec(vec![3])).run(&mut ())),
          Ok(())
        );
        gate_tx.send(()).expect("open gate for consumer");
        assert_eq!(block_on(end_stream(s2).run(&mut ())), Ok(()));
      });
      gate_rx.recv().expect("producer queued three chunks");
      let out = block_on(stream.run_collect().run(&mut ()));
      producer.join().expect("producer thread");
      assert_eq!(out, Ok(vec![1, 2]));
    }

    #[test]
    fn stream_sender_end_stream_closes_queue() {
      let (mut stream, sender) = stream_from_channel::<i32, (), ()>(2);
      assert_eq!(
        block_on(send_chunk(&sender, Chunk::from_vec(vec![42])).run(&mut ())),
        Ok(())
      );
      assert_eq!(block_on(end_stream(sender).run(&mut ())), Ok(()));
      assert_eq!(
        block_on(stream.poll_next_chunk(&mut ())),
        Ok(Some(Chunk::from_vec(vec![42])))
      );
      assert_eq!(block_on(stream.poll_next_chunk(&mut ())), Ok(None));
    }
  }

  mod transformations {
    use super::*;
    use crate::foundation::predicate::predicate;
    use crate::observability::metric::Metric;

    #[test]
    fn stream_throughput_metric_counts_elements() {
      let m = Metric::counter("stream_throughput_elems", []);
      let stream = Stream::range(0, 5).with_throughput_metric(m.clone());
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![0, 1, 2, 3, 4]));
      assert_eq!(m.snapshot_count(), 5);
    }

    #[test]
    fn map_filter_take_chain_with_values_produces_expected_output() {
      let stream = Stream::from_iterable(1..=10)
        .filter(Box::new(|n: &i32| *n % 2 == 0))
        .map(|n| n * 10)
        .take(3);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![20, 40, 60]));
    }

    #[test]
    fn default_map_matches_serial_for_from_iterable() {
      let a = block_on(
        Stream::from_iterable(1..=20i32)
          .map(|n| n * 2)
          .run_collect()
          .run(&mut ()),
      );
      let b = block_on(
        Stream::from_iterable(1..=20i32)
          .map_serial(|n| n * 2)
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(a, b);
    }

    #[test]
    fn default_filter_matches_serial_for_from_iterable() {
      let p = Box::new(|n: &i32| *n % 2 == 0);
      let a = block_on(
        Stream::from_iterable(0..=30i32)
          .filter(p.clone())
          .run_collect()
          .run(&mut ()),
      );
      let b = block_on(
        Stream::from_iterable(0..=30i32)
          .filter_serial(p)
          .run_collect()
          .run(&mut ()),
      );
      let mut a = a.expect("a");
      let mut b = b.expect("b");
      a.sort();
      b.sort();
      assert_eq!(a, b);
    }

    #[test]
    fn grouped_with_size_groups_stream_into_fixed_size_batches() {
      let stream = Stream::from_iterable([1, 2, 3, 4, 5]).grouped(2);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![vec![1, 2], vec![3, 4], vec![5]]));
    }

    #[test]
    fn grouped_with_zero_size_returns_empty_output() {
      let stream = Stream::from_iterable([1, 2, 3]).grouped(0);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(Vec::<Vec<i32>>::new()));
    }

    #[test]
    fn map_effect_with_mapping_function_lifts_async_effectful_mapping() {
      let stream =
        Stream::from_iterable([1, 2, 3]).map_effect(|n| succeed::<i32, &'static str, ()>(n * 2));
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![2, 4, 6]));
    }

    #[test]
    fn map_effect_with_failures_preserves_upstream_and_mapper_error_channels() {
      let upstream_fail = Stream::<i32, &'static str, ()>::from_effect(fail("upstream"))
        .map_effect(|n| succeed::<i32, &'static str, ()>(n * 2));
      let upstream_out = block_on(upstream_fail.run_collect().run(&mut ()));
      assert_eq!(upstream_out, Err(Or::Left("upstream")));

      let mapper_fail =
        Stream::from_iterable([1, 2, 3]).map_effect(|_n| fail::<i32, &'static str, ()>("mapper"));
      let mapper_out = block_on(mapper_fail.run_collect().run(&mut ()));
      assert_eq!(mapper_out, Err(Or::Right("mapper")));
    }

    #[test]
    fn pure_transforms_with_incremental_polling_drive_output_from_chunks() {
      let mut stream = Stream::from_iterable([1, 2, 3, 4, 5, 6])
        .map(|n| n * 2)
        .filter(Box::new(|n: &i32| *n % 4 == 0))
        .take(2);
      let mut env = ();
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 1)),
        Ok(Some(Chunk::from_vec(vec![4])))
      );
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 1)),
        Ok(Some(Chunk::from_vec(vec![8])))
      );
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 1)),
        Ok(None)
      );
    }

    #[rstest]
    #[case(vec![1, 2, 3, 4, 5, 6], vec![2, 4, 6])]
    #[case(vec![2, 4, 8], vec![2, 4, 8])]
    fn filter_keeps_matching_elements(#[case] input: Vec<i32>, #[case] expected: Vec<i32>) {
      let stream = Stream::from_iterable(input).filter(Box::new(|n: &i32| *n % 2 == 0));
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(expected));
    }

    #[test]
    fn filter_empty_stream_returns_empty() {
      let stream = Stream::<i32, (), ()>::from_iterable(core::iter::empty())
        .filter(Box::new(|n: &i32| *n % 2 == 0));
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(Vec::new()));
    }

    #[test]
    fn take_while_stops_at_first_false() {
      let stream =
        Stream::from_iterable([2_i32, 4, 5, 6, 8]).take_while(Box::new(|n: &i32| *n % 2 == 0));
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![2, 4]));
    }

    #[test]
    fn drop_while_skips_initial_run() {
      let stream =
        Stream::from_iterable([0_i32, 2, 4, 1, 2, 3]).drop_while(Box::new(|n: &i32| *n % 2 == 0));
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![1, 2, 3]));
    }

    #[rstest]
    #[case(
      vec![-4_i32, -2, 0, 1, 2, 3, 4, 5],
      vec![2_i32, 4]
    )]
    #[case(vec![1_i32, 3, 5], vec![])]
    #[case(vec![2_i32, 4, 6], vec![2_i32, 4, 6])]
    fn filter_with_predicate_and_combinator_matches_only_even_positive(
      #[case] input: Vec<i32>,
      #[case] expected: Vec<i32>,
    ) {
      let p = predicate::and(Box::new(|n: &i32| *n % 2 == 0), Box::new(|n: &i32| *n > 0));
      let stream = Stream::from_iterable(input).filter(p);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(expected));
    }
  }

  mod consumers {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn run_fold_with_values_accumulates_all_values() {
      let stream = Stream::from_iterable([1, 2, 3, 4]);
      let out = block_on(stream.run_fold(0, |acc, x| acc + x).run(&mut ()));
      assert_eq!(out, Ok(10));
    }

    #[test]
    fn run_collect_with_pre_cancelled_token_returns_empty_output() {
      let stream = Stream::<i32, (), CancellationToken>::from_effect(succeed(vec![1, 2, 3]));
      let mut token = CancellationToken::new();
      token.cancel();
      let out = block_on(stream.run_collect().run(&mut token));
      assert_eq!(out, Ok(Vec::<i32>::new()));
    }

    #[test]
    fn run_reduce_with_empty_stream_returns_none() {
      let stream = Stream::<i32, (), ()>::from_iterable(core::iter::empty());
      let out = block_on(stream.run_reduce(|a, b| a + b).run(&mut ()));
      assert_eq!(out, Ok(None));
    }

    #[rstest]
    #[case::sum(vec![1, 2, 3], 6)]
    #[case::single(vec![5], 5)]
    fn run_reduce_with_values_combines_values_when_present(
      #[case] input: Vec<i32>,
      #[case] expected: i32,
    ) {
      let stream = Stream::from_iterable(input);
      let out = block_on(stream.run_reduce(|a, b| a + b).run(&mut ()));
      assert_eq!(out, Ok(Some(expected)));
    }

    #[test]
    fn run_for_each_with_callback_executes_sync_effect_for_each_element() {
      let seen: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
      let seen_ref = Rc::clone(&seen);
      let stream = Stream::from_iterable([2, 4, 6]);
      let out = block_on(
        stream
          .run_for_each(move |n| {
            let seen_ref = Rc::clone(&seen_ref);
            Effect::new(move |_r: &mut ()| {
              seen_ref.borrow_mut().push(n);
              Ok(())
            })
          })
          .run(&mut ()),
      );
      assert_eq!(out, Ok(()));
      assert_eq!(*seen.borrow(), vec![2, 4, 6]);
    }
  }

  mod unfold_effect {
    use super::*;

    #[test]
    fn unfold_effect_with_generator_pulls_until_none() {
      let stream = Stream::unfold_effect(0, |s| {
        succeed::<Option<(i32, i32)>, (), ()>(if s < 3 { Some((s, s + 1)) } else { None })
      });
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![0, 1, 2]));
    }
  }

  mod map_effect_parallel {
    use super::*;
    use crate::run_async;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn map_effect_preserves_order() {
      let stream = Stream::from_iterable([1, 2, 3, 4, 5]);
      let out = run_async(
        stream
          .map_par_n(2, |x: i32| {
            Effect::new_async(move |_r: &mut ()| {
              crate::box_future(async move {
                tokio::time::sleep(std::time::Duration::from_millis((6 - x).max(1) as u64)).await;
                Ok(x * 10)
              })
            })
          })
          .run_collect(),
        (),
      )
      .await
      .expect("collect");
      assert_eq!(out, vec![10, 20, 30, 40, 50]);
    }

    #[tokio::test]
    async fn map_effect_limits_concurrency() {
      let current = Arc::new(AtomicUsize::new(0));
      let max_seen = Arc::new(AtomicUsize::new(0));
      let stream = Stream::from_iterable(0..12usize);
      let c = Arc::clone(&current);
      let m = Arc::clone(&max_seen);
      let collected = run_async(
        stream
          .map_par_n(3, move |i: usize| {
            let c = Arc::clone(&c);
            let m = Arc::clone(&m);
            Effect::new_async(move |_r: &mut ()| {
              crate::box_future(async move {
                let active = c.fetch_add(1, Ordering::SeqCst) + 1;
                let mut prev = m.load(Ordering::SeqCst);
                while active > prev {
                  match m.compare_exchange_weak(prev, active, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(_) => break,
                    Err(p) => prev = p,
                  }
                }
                tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                c.fetch_sub(1, Ordering::SeqCst);
                Ok(i)
              })
            })
          })
          .run_collect(),
        (),
      )
      .await
      .expect("collect");
      assert_eq!(collected.len(), 12);
      assert!(
        max_seen.load(Ordering::SeqCst) <= 3,
        "observed max concurrency {}",
        max_seen.load(Ordering::SeqCst)
      );
    }

    #[tokio::test]
    async fn map_par_adaptive_collects() {
      use crate::compute::{ComputeFabric, install_fabric};
      use std::sync::Arc;

      install_fabric(Arc::new(ComputeFabric::memory_cap_max_cpu(1.0)));
      let stream = Stream::from_iterable([1, 2, 3, 4]);
      let out = run_async(
        stream
          .map_par_adaptive(|x: i32| Effect::new(move |_r: &mut ()| Ok(x * 2)))
          .run_collect(),
        (),
      )
      .await
      .expect("collect");
      assert_eq!(out, vec![2, 4, 6, 8]);
    }

    #[tokio::test]
    async fn map_effect_propagates_error_from_inner() {
      let stream = Stream::from_iterable([1, 2, 3]);
      let res = run_async(
        stream
          .map_par_n(2, |x: i32| {
            Effect::new(move |_r: &mut ()| if x == 2 { Err(()) } else { Ok(x) })
          })
          .run_collect(),
        (),
      )
      .await;
      assert_eq!(res, Err(()));
    }
  }

  mod channel_backed {
    use super::*;
    use crate::coordination::channel::{Channel, QueueChannel};

    #[test]
    fn stream_map_via_channel_preserves_elements() {
      let ch = Channel::<i32, (), (), (), ()>::from_stream(Stream::from_iterable([1_i32, 2, 3]));
      let stream = Stream::from_channel(ch).map(|n| n * 2);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![2, 4, 6]));
    }

    #[test]
    fn stream_filter_via_channel_drops_unmatched() {
      let ch = Channel::<i32, (), (), (), ()>::from_stream(Stream::from_iterable([1_i32, 2, 3, 4]));
      let stream = Stream::from_channel(ch).filter(Box::new(|n: &i32| *n % 2 == 0));
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![2, 4]));
    }

    #[test]
    fn stream_collect_via_channel_gathers_all() {
      let ch = Channel::<i32, (), (), (), ()>::from_stream(Stream::from_iterable([10_i32, 20, 30]));
      let stream = Stream::from_channel(ch);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![10, 20, 30]));
    }

    #[test]
    fn duplex_queue_channel_to_stream_drains_after_shutdown() {
      use crate::runtime::run_blocking;
      let ch = run_blocking(QueueChannel::<i32, i32, ()>::duplex_unbounded(), ()).expect("channel");
      run_blocking(ch.write(1), ()).unwrap();
      run_blocking(ch.write(2), ()).unwrap();
      run_blocking(ch.shutdown(), ()).unwrap();
      let out = block_on(ch.to_stream().map(|x| x * 10).run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![10, 20]));
    }

    #[test]
    fn stream_from_duplex_queue_channel_matches_queue_channel_to_stream() {
      use crate::runtime::run_blocking;
      let mk = || {
        let ch =
          run_blocking(QueueChannel::<i32, i32, ()>::duplex_unbounded(), ()).expect("channel");
        run_blocking(ch.write(7), ()).unwrap();
        run_blocking(ch.shutdown(), ()).unwrap();
        ch
      };
      let ch_a = mk();
      let ch_b = mk();
      let a = block_on(
        Stream::from_duplex_queue_channel(ch_a)
          .run_collect()
          .run(&mut ()),
      );
      let b = block_on(ch_b.to_stream().run_collect().run(&mut ()));
      assert_eq!(a, Ok(vec![7]));
      assert_eq!(a, b);
    }
  }

  mod broadcast_tests {
    use super::*;
    use crate::run_async;
    use std::time::Duration;

    #[tokio::test]
    async fn broadcast_all_consumers_receive_every_element() {
      let src = Stream::from_iterable(vec![1_i32, 2, 3]);
      let (streams, pump) = run_async(src.broadcast(8, 2), ()).await.expect("broadcast");
      assert_eq!(streams.len(), 2);
      let mut streams = streams;
      let s1 = streams.pop().expect("s1");
      let s0 = streams.pop().expect("s0");
      let (pr, a, b) = tokio::join!(
        run_async(pump, ()),
        run_async(s0.run_collect(), ()),
        run_async(s1.run_collect(), ()),
      );
      pr.expect("pump");
      assert_eq!(a.expect("collect 0"), vec![1, 2, 3]);
      assert_eq!(b.expect("collect 1"), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn broadcast_slow_consumer_does_not_block_fast() {
      let (src, sender) = stream_from_channel::<i32, (), ()>(32);
      let (streams, pump) = run_async(src.broadcast(64, 2), ())
        .await
        .expect("broadcast");
      let mut streams = streams;
      let s_slow = streams.pop().expect("slow stream");
      let s_fast = streams.pop().expect("fast stream");

      let (pr, _, fast_v, slow_v) = tokio::join!(
        run_async(pump, ()),
        async {
          for i in 0..30i32 {
            run_async(send_chunk(&sender, Chunk::from_vec(vec![i])), ())
              .await
              .expect("send");
          }
          run_async(end_stream(sender), ()).await.expect("end");
        },
        run_async(s_fast.run_collect(), ()),
        async move {
          let mut out = Vec::new();
          let mut s = s_slow;
          let mut env = ();
          loop {
            tokio::time::sleep(Duration::from_millis(8)).await;
            match s.poll_next_chunk(&mut env).await.expect("poll") {
              Some(c) => out.extend(c.into_vec()),
              None => break,
            }
          }
          out
        },
      );
      pr.expect("pump");
      assert_eq!(fast_v.expect("fast collect"), (0..30).collect::<Vec<_>>());
      assert_eq!(slow_v, (0..30).collect::<Vec<_>>());
    }
  }

  // ── from_effect ──────────────────────────────────────────────────────────

  mod from_effect {
    use super::*;

    #[test]
    fn from_effect_wraps_vec_producing_effect() {
      let eff = succeed::<Vec<i32>, (), ()>(vec![10, 20, 30]);
      let stream = Stream::from_effect(eff);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![10, 20, 30]));
    }

    #[test]
    fn from_effect_propagates_failure() {
      let eff = fail::<Vec<i32>, &'static str, ()>("boom");
      let stream = Stream::from_effect(eff);
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Err("boom"));
    }
  }

  // ── run_fold_effect ──────────────────────────────────────────────────────

  mod run_fold_effect {
    use super::*;

    #[test]
    fn run_fold_effect_accumulates_with_effectful_step() {
      let stream = Stream::from_iterable(vec![1_i32, 2, 3]);
      let out = block_on(
        stream
          .run_fold_effect(0, |acc, x| succeed::<i32, (), ()>(acc + x))
          .run(&mut ()),
      );
      assert_eq!(out, Ok(6));
    }

    #[test]
    fn run_fold_effect_propagates_step_failure() {
      let stream =
        Stream::<i32, &'static str, ()>::from_effect(succeed::<Vec<i32>, &'static str, ()>(vec![
          1, 2, 3,
        ]));
      let out = block_on(
        stream
          .run_fold_effect(0_i32, |_acc, x| {
            if x == 2 {
              fail::<i32, &'static str, ()>("step fail")
            } else {
              succeed::<i32, &'static str, ()>(0)
            }
          })
          .run(&mut ()),
      );
      assert_eq!(out, Err("step fail"));
    }
  }

  // ── scan ─────────────────────────────────────────────────────────────────

  mod scan {
    use super::*;

    #[test]
    fn scan_emits_running_state_per_element() {
      let stream = Stream::from_iterable(vec![1_i32, 2, 3, 4]);
      let out = block_on(
        stream
          .scan(0_i32, |acc, x| {
            *acc += x;
            *acc
          })
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![1, 3, 6, 10]));
    }

    #[test]
    fn scan_with_empty_stream_produces_empty_output() {
      let stream = Stream::<i32, (), ()>::from_iterable(vec![]);
      let out = block_on(
        stream
          .scan(0_i32, |acc, x| {
            *acc += x;
            *acc
          })
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![]));
    }
  }

  mod run_for_each_effect_direct {
    use super::*;

    #[test]
    fn run_for_each_effect_collects_side_effects() {
      use std::sync::{Arc, Mutex};
      let collected = Arc::new(Mutex::new(vec![]));
      let c = Arc::clone(&collected);
      let stream = Stream::from_iterable(vec![1_i32, 2, 3]);
      let result = block_on(
        stream
          .run_for_each_effect(move |x| {
            c.lock().unwrap().push(x);
            succeed::<(), (), ()>(())
          })
          .run(&mut ()),
      );
      assert_eq!(result, Ok(()));
      assert_eq!(*collected.lock().unwrap(), vec![1, 2, 3]);
    }
  }

  mod stream_channel_full_error {
    use super::*;

    #[test]
    fn stream_channel_full_display() {
      let e = StreamChannelFull;
      let s = format!("{e}");
      assert!(s.contains("full"), "display: {s}");
    }

    #[test]
    fn stream_channel_full_debug() {
      let _ = format!("{:?}", StreamChannelFull);
    }

    #[test]
    fn stream_channel_full_is_error_trait() {
      let e: &dyn std::error::Error = &StreamChannelFull;
      let _ = format!("{e}");
    }

    #[test]
    fn send_chunk_fail_policy_returns_err_when_full() {
      // Use a 2-item queue: fill it, then verify Fail policy rejects immediately.
      // Drop sender/stream directly — no end_stream (which would loop on full queue).
      let (_stream, sender) =
        stream_from_channel_with_policy::<i32, (), ()>(2, BackpressurePolicy::Fail);
      let r1 = block_on(send_chunk(&sender, Chunk::from_vec(vec![1])).run(&mut ()));
      assert_eq!(r1, Ok(()));
      let r2 = block_on(send_chunk(&sender, Chunk::from_vec(vec![2])).run(&mut ()));
      assert_eq!(r2, Ok(()));
      // Queue full — Fail policy returns Err immediately
      let r3 = block_on(send_chunk(&sender, Chunk::from_vec(vec![3])).run(&mut ()));
      assert_eq!(r3, Err(StreamChannelFull));
    }
  }

  mod range_tests {
    use super::*;

    #[test]
    fn range_empty_when_start_equals_end() {
      let out = block_on(
        Stream::<i64, (), ()>::range(5, 5)
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![]));
    }

    #[test]
    fn range_empty_when_start_greater_than_end() {
      let out = block_on(
        Stream::<i64, (), ()>::range(10, 5)
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![]));
    }
  }

  mod stream_error_channel {
    use super::*;

    #[test]
    fn stream_error_propagates_via_fail_message() {
      // Inject a ChannelMessage::Fail directly via the sender's queue
      let (stream, sender) = stream_from_channel_with_policy::<i32, &'static str, ()>(
        8,
        BackpressurePolicy::BoundedBlock,
      );
      let s = sender.clone();
      let _ = block_on(send_chunk(&s, Chunk::from_vec(vec![1, 2])).run(&mut ()));
      let msg = ChannelMessage::Fail("stream error");
      let _ = crate::runtime::run_blocking(sender.queue.offer(msg), ());
      let out = block_on(stream.run_collect().run(&mut ()));
      assert_eq!(out, Err("stream error"));
    }
  }

  // ── pull-based / infinite sources ───────────────────────────────────────

  mod pull_based {
    use super::*;
    use crate::testing::assert_bounded_memory;
    use std::cell::Cell;
    use std::rc::Rc;

    /// Infinite counter source that records the total number of items requested from it.
    fn counting_source(requested: Rc<Cell<usize>>) -> Stream<u64, (), ()> {
      let mut next = 0_u64;
      Stream::from_pull(move |_r: &mut (), n| {
        requested.set(requested.get() + n);
        let batch: Vec<u64> = (next..next + n as u64).collect();
        next += n as u64;
        Box::pin(async move { Ok(Some(batch)) })
      })
    }

    #[test]
    fn from_pull_splits_oversized_batches_to_requested_chunk_size() {
      let mut done = false;
      let mut stream = Stream::<i32, (), ()>::from_pull(move |_r: &mut (), _n| {
        let batch = (!done).then(|| vec![1, 2, 3, 4, 5]);
        done = true;
        Box::pin(async move { Ok(batch) })
      });
      let mut env = ();
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 2)),
        Ok(Some(Chunk::from_vec(vec![1, 2])))
      );
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 2)),
        Ok(Some(Chunk::from_vec(vec![3, 4])))
      );
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 2)),
        Ok(Some(Chunk::from_vec(vec![5])))
      );
      assert_eq!(
        block_on(stream.poll_next_chunk_with_size(&mut env, 2)),
        Ok(None)
      );
    }

    #[test]
    fn from_pull_skips_empty_batches_and_propagates_errors() {
      let mut calls = 0;
      let stream = Stream::<i32, &'static str, ()>::from_pull(move |_r: &mut (), _n| {
        calls += 1;
        let res = match calls {
          1 | 2 => Ok(Some(Vec::new())),
          3 => Ok(Some(vec![7])),
          _ => Err("boom"),
        };
        Box::pin(async move { res })
      });
      let mut stream = stream;
      assert_eq!(
        block_on(stream.poll_next_chunk(&mut ())),
        Ok(Some(Chunk::from_vec(vec![7])))
      );
      assert_eq!(block_on(stream.poll_next_chunk(&mut ())), Err("boom"));
    }

    #[test]
    fn map_effect_failure_fuses_stream() {
      let mut stream = Stream::from_iterable(0..200).map_effect(|n: i32| {
        if n == 0 {
          fail::<i32, &'static str, ()>("bad")
        } else {
          succeed::<i32, &'static str, ()>(n)
        }
      });
      assert_eq!(
        block_on(stream.poll_next_chunk(&mut ())),
        Err(Or::Right("bad"))
      );
      assert_eq!(block_on(stream.poll_next_chunk(&mut ())), Ok(None));
    }

    /// Source failing on its first pull and yielding `[1, 2]` on any later pull.
    fn fail_then_items() -> Stream<i32, &'static str, ()> {
      let mut calls = 0;
      Stream::from_pull(move |_r: &mut (), _n| {
        calls += 1;
        let res = if calls == 1 {
          Err("first")
        } else {
          Ok(Some(vec![1, 2]))
        };
        Box::pin(async move { res })
      })
    }

    #[test]
    fn upstream_failure_fuses_pull_stage() {
      let mut stream = fail_then_items().map(|n| n * 10);
      assert_eq!(block_on(stream.poll_next_chunk(&mut ())), Err("first"));
      assert_eq!(block_on(stream.poll_next_chunk(&mut ())), Ok(None));
    }

    #[test]
    fn chain_failure_in_first_fuses_stream() {
      let second = Stream::<i32, &'static str, ()>::from_effect(succeed(vec![9]));
      let mut stream = fail_then_items().chain(second);
      assert_eq!(block_on(stream.poll_next_chunk(&mut ())), Err("first"));
      assert_eq!(block_on(stream.poll_next_chunk(&mut ())), Ok(None));
    }

    #[test]
    fn chain_failure_in_second_fuses_stream() {
      let first = Stream::<i32, &'static str, ()>::from_effect(succeed(vec![9]));
      let mut stream = first.chain(fail_then_items());
      assert_eq!(
        block_on(stream.poll_next_chunk(&mut ())),
        Ok(Some(Chunk::from_vec(vec![9])))
      );
      assert_eq!(block_on(stream.poll_next_chunk(&mut ())), Err("first"));
      assert_eq!(block_on(stream.poll_next_chunk(&mut ())), Ok(None));
    }

    /// Infinite counter source over a [`CancellationToken`] environment.
    fn token_source() -> Stream<u64, (), CancellationToken> {
      let mut next = 0_u64;
      Stream::from_pull(move |_r: &mut CancellationToken, n| {
        let batch: Vec<u64> = (next..next + n as u64).collect();
        next += n as u64;
        Box::pin(async move { Ok(Some(batch)) })
      })
    }

    #[test]
    fn empty_batches_honour_cancellation() {
      let mut token = CancellationToken::new();
      token.cancel();
      let mut stream = token_source().filter(Box::new(|_: &u64| false));
      // Without the cancellation check this poll would spin forever on empty batches.
      assert_eq!(block_on(stream.poll_next_chunk(&mut token)), Ok(None));
      let out = block_on(
        token_source()
          .filter(Box::new(|_: &u64| false))
          .run_collect()
          .run(&mut token),
      );
      assert_eq!(out, Ok(Vec::new()));
    }

    #[test]
    fn map_effect_is_lazy_until_first_pull() {
      use std::sync::atomic::{AtomicUsize, Ordering};
      let pulls = Arc::new(AtomicUsize::new(0));
      let p = Arc::clone(&pulls);
      let stream = Stream::<i32, (), ()>::from_pull(move |_r: &mut (), _n| {
        p.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(Some(vec![1])) })
      })
      .map_effect(|n| succeed::<i32, (), ()>(n));
      assert_eq!(pulls.load(Ordering::SeqCst), 0);
      let out = block_on(stream.take(1).run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![1]));
      assert_eq!(pulls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn from_pull_counts_throughput() {
      let m = Metric::counter("stream_pull_throughput", []);
      let out = block_on(
        Stream::from_iterable(0..10)
          .with_throughput_metric(m.clone())
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok((0..10).collect::<Vec<_>>()));
      assert_eq!(m.snapshot_count(), 10);
    }

    #[test]
    fn take_stops_pulling_upstream_once_n_reached() {
      let requested = Rc::new(Cell::new(0));
      let out = block_on(
        counting_source(Rc::clone(&requested))
          .take(5)
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![0, 1, 2, 3, 4]));
      assert_eq!(requested.get(), 5, "take must not request past n");
    }

    #[test]
    fn take_while_stops_pulling_at_first_false() {
      let requested = Rc::new(Cell::new(0));
      let out = block_on(
        counting_source(Rc::clone(&requested))
          .take_while(Box::new(|n: &u64| *n < 3))
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![0, 1, 2]));
      assert_eq!(requested.get(), 64, "exactly one upstream chunk pulled");
    }

    #[test]
    fn map_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(|s| s.map(|n| n * 3), 5, 64);
      assert_eq!(out, vec![0, 3, 6, 9, 12]);
    }

    #[test]
    fn map_serial_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(|s| s.map_serial(|n| n + 1), 4, 64);
      assert_eq!(out, vec![1, 2, 3, 4]);
    }

    #[test]
    fn filter_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(|s| s.filter(Box::new(|n: &u64| n % 2 == 1)), 4, 64);
      assert_eq!(out, vec![1, 3, 5, 7]);
    }

    #[test]
    fn filter_serial_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(
        |s| s.filter_serial(Box::new(|n: &u64| n.is_multiple_of(3))),
        4,
        64,
      );
      assert_eq!(out, vec![0, 3, 6, 9]);
    }

    #[test]
    fn take_while_on_infinite_source_terminates() {
      let out = assert_bounded_memory(|s| s.take_while(Box::new(|n: &u64| *n < 100)), 1000, 64);
      assert_eq!(out, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn drop_while_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(|s| s.drop_while(Box::new(|n: &u64| *n < 70)), 3, 128);
      assert_eq!(out, vec![70, 71, 72]);
    }

    #[test]
    fn take_on_infinite_source_terminates() {
      let out = assert_bounded_memory(|s| s.take(200), 1000, 64);
      assert_eq!(out, (0..200).collect::<Vec<_>>());
    }

    #[test]
    fn grouped_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(|s| s.grouped(3), 2, 64);
      assert_eq!(out, vec![vec![0, 1, 2], vec![3, 4, 5]]);
    }

    #[test]
    fn scan_on_infinite_source_is_incremental() {
      let out = assert_bounded_memory(
        |s| {
          s.scan(0_u64, |acc, x| {
            *acc += x;
            *acc
          })
        },
        5,
        64,
      );
      assert_eq!(out, vec![0, 1, 3, 6, 10]);
    }

    #[test]
    fn chained_operators_on_infinite_source_stay_bounded() {
      let out = assert_bounded_memory(
        |s| {
          s.map(|n| n * 2)
            .filter(Box::new(|n: &u64| n.is_multiple_of(3)))
            .grouped(2)
            .map(|g| g.iter().sum::<u64>())
        },
        3,
        64,
      );
      assert_eq!(out, vec![6, 30, 54]);
    }

    #[test]
    fn map_effect_on_infinite_source_is_incremental() {
      let requested = Rc::new(Cell::new(0));
      let out = block_on(
        counting_source(Rc::clone(&requested))
          .map_effect(|n| succeed::<u64, (), ()>(n + 10))
          .take(4)
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![10, 11, 12, 13]));
      assert_eq!(requested.get(), 4);
    }

    #[test]
    fn from_iterable_over_infinite_iterator_is_lazy() {
      let out = block_on(
        Stream::from_iterable(0_u64..)
          .take(3)
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![0, 1, 2]));
    }

    #[test]
    fn unfold_infinite_generator_is_lazy() {
      let out = block_on(
        Stream::unfold(1_u64, |s| Some((s, s * 2)))
          .take(5)
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![1, 2, 4, 8, 16]));
    }

    #[test]
    fn unfold_effect_infinite_generator_is_lazy() {
      let out = block_on(
        Stream::unfold_effect(0_u64, |s| {
          succeed::<Option<(u64, u64)>, (), ()>(Some((s, s + 1)))
        })
        .take(4)
        .run_collect()
        .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![0, 1, 2, 3]));
    }

    #[test]
    fn range_with_huge_bound_is_lazy() {
      let out = block_on(
        Stream::range(0, i64::MAX)
          .take(3)
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![0, 1, 2]));
    }

    #[test]
    fn from_duplex_queue_channel_reads_only_what_is_pulled() {
      use crate::coordination::channel::QueueChannel;
      use crate::runtime::run_blocking;
      // No shutdown: an eager drain would block forever waiting for a fourth item.
      let ch = run_blocking(QueueChannel::<i32, i32, ()>::duplex_unbounded(), ()).expect("channel");
      for i in 1..=3 {
        run_blocking(ch.write(i), ()).unwrap();
      }
      let out = block_on(
        Stream::from_duplex_queue_channel(ch)
          .take(3)
          .run_collect()
          .run(&mut ()),
      );
      assert_eq!(out, Ok(vec![1, 2, 3]));
    }

    #[test]
    fn from_channel_reads_only_what_is_pulled() {
      use crate::coordination::channel::Channel;
      let ch = Channel::<u64, (), (), (), ()>::from_stream(Stream::from_iterable(0_u64..));
      let out = block_on(Stream::from_channel(ch).take(3).run_collect().run(&mut ()));
      assert_eq!(out, Ok(vec![0, 1, 2]));
    }
  }

  mod pull_based_props {
    use crate::testing::assert_bounded_memory;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    enum Op {
      Add(u64),
      /// Drop one residue class `x % m == r` (`m >= 5`, so a chain of ≤ 4 filters always keeps
      /// infinitely many elements).
      Reject(u64, u64),
      Take(usize),
    }

    fn op() -> impl Strategy<Value = Op> {
      prop_oneof![
        (0_u64..100).prop_map(Op::Add),
        (5_u64..9)
          .prop_flat_map(|m| (Just(m), 0..m))
          .prop_map(|(m, r)| Op::Reject(m, r)),
        (0_usize..300).prop_map(Op::Take),
      ]
    }

    proptest! {
      #![proptest_config(ProptestConfig::with_cases(64))]

      #[test]
      fn infinite_source_with_map_filter_take_chain_yields_first_n(
        ops in proptest::collection::vec(op(), 0..=4),
        n in 0_usize..200,
      ) {
        let mut expected: Box<dyn Iterator<Item = u64>> = Box::new(0_u64..);
        for o in ops.clone() {
          expected = match o {
            Op::Add(k) => Box::new(expected.map(move |x| x + k)),
            Op::Reject(m, r) => Box::new(expected.filter(move |x| x % m != r)),
            Op::Take(k) => Box::new(expected.take(k)),
          };
        }
        let expected: Vec<u64> = expected.take(n).collect();

        let out = assert_bounded_memory(
          move |s| {
            ops.into_iter().fold(s, |s, o| match o {
              Op::Add(k) => s.map(move |x| x + k),
              Op::Reject(m, r) => s.filter(Box::new(move |x: &u64| x % m != r)),
              Op::Take(k) => s.take(k),
            })
          },
          n,
          128,
        );
        prop_assert_eq!(out, expected);
      }
    }
  }
}
