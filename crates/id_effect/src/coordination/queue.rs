//! [`Queue`]: an MPMC queue with bounded (back-pressure), dropping, sliding and unbounded
//! strategies (Effect.ts `Queue`). Ported from the v1 `coordination::queue` (flume plus tokio
//! sync), on one mutex and two FIFO wait lists.
//!
//! - A taker suspends while the queue is empty; an offer hands its item straight to the front
//!   taker. An interrupted taker is removed, and an item handed to it but not yet observed goes
//!   back to the head of the queue.
//! - On a full bounded queue [`Queue::offer`] suspends until a slot frees up (Effect.ts
//!   back-pressure); [`Queue::try_offer`] returns `false` instead (the v1 `offer`). A dropping
//!   queue refuses the new item, a sliding queue evicts the oldest one.
//! - [`Queue::shutdown`] closes the queue to producers: pending and later offers return `false`,
//!   buffered items stay available, and takers fail with [`QueueError::Disconnected`] once the
//!   buffer is drained (the v1 behaviour).

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use super::waiters::{Signal, WaitList, lock, wake_all};
use crate::kernel::{Effect, fail, from_async, succeed, suspend, sync};

/// A take on a queue that is shut down and drained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
  /// The queue is shut down and its buffer is empty.
  Disconnected,
}

impl fmt::Display for QueueError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      QueueError::Disconnected => write!(f, "queue is disconnected"),
    }
  }
}

impl std::error::Error for QueueError {}

/// What a full queue does with a new item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strategy {
  /// The offer suspends until a slot frees up.
  Bounded,
  /// The new item is refused.
  Dropping,
  /// The oldest item is evicted.
  Sliding,
  /// Never full.
  Unbounded,
}

/// The outcome of putting one item.
enum Put<A> {
  Stored,
  Refused(A),
  Full(A),
}

pub(crate) struct State<A> {
  buf: VecDeque<A>,
  cap: usize,
  strategy: Strategy,
  shutdown: bool,
  takers: WaitList<(), A>,
  /// Payload: the item to store. Grant: `None` stored, `Some(item)` refused by a shutdown.
  offerers: WaitList<A, Option<A>>,
}

impl<A> State<A> {
  /// Moves blocked offers into free slots.
  fn admit_offerers(&mut self, ws: &mut Vec<Waker>) {
    while self.buf.len() < self.cap {
      let Some((id, item, w)) = self.offerers.pop_front() else {
        break;
      };
      self.buf.push_back(item);
      self.offerers.grant(id, None);
      ws.push(w);
    }
  }

  /// Delivers `item` to the front taker, or buffers it per the strategy.
  fn put(&mut self, item: A, ws: &mut Vec<Waker>) -> Put<A> {
    if self.shutdown {
      return Put::Refused(item);
    }
    let item = match self.takers.grant_front(item) {
      Ok(((), w)) => {
        ws.push(w);
        return Put::Stored;
      }
      Err(item) => item,
    };
    if self.buf.len() < self.cap {
      self.buf.push_back(item);
      return Put::Stored;
    }
    match self.strategy {
      Strategy::Sliding => {
        self.buf.pop_front();
        self.buf.push_back(item);
        Put::Stored
      }
      Strategy::Dropping => Put::Refused(item),
      Strategy::Bounded => Put::Full(item),
      Strategy::Unbounded => {
        self.buf.push_back(item);
        Put::Stored
      }
    }
  }

  /// Takes the head item, admitting a blocked offer into the freed slot.
  fn pop(&mut self, ws: &mut Vec<Waker>) -> Option<A> {
    let v = self.buf.pop_front()?;
    self.admit_offerers(ws);
    Some(v)
  }

  /// Returns an item that was taken but never observed: to the front taker, else to the head.
  fn put_back(&mut self, item: A, ws: &mut Vec<Waker>) {
    match self.takers.grant_front(item) {
      Ok(((), w)) => ws.push(w),
      Err(item) => self.buf.push_front(item),
    }
  }
}

/// Cache-line aligned, so the `Arc` counts that every operation's handle clone touches do not
/// share a line with the lock a producer and a consumer on other threads contend for.
#[repr(align(128))]
pub(crate) struct Inner<A> {
  state: Mutex<State<A>>,
  closed: Arc<Signal>,
}

/// A multi-producer, multi-consumer queue. Cloning yields another handle to the same queue.
///
/// ```
/// use id_effect::{Queue, QueueError};
///
/// let q = Queue::<u32>::bounded(2);
/// let eff = q
///   .offer::<QueueError, _>(1)
///   .then(q.offer(2))
///   .then(q.take_all());
/// assert_eq!(eff.block_on().into_result().ok(), Some(vec![1, 2]));
/// ```
pub struct Queue<A> {
  inner: Arc<Inner<A>>,
}

impl<A> Clone for Queue<A> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<A> fmt::Debug for Queue<A> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let st = lock(&self.inner.state);
    f.debug_struct("Queue")
      .field("strategy", &st.strategy)
      .field("size", &st.buf.len())
      .field("shutdown", &st.shutdown)
      .finish()
  }
}

impl<A: Send + 'static> Queue<A> {
  fn with(strategy: Strategy, cap: usize) -> Self {
    Self {
      inner: Arc::new(Inner {
        state: Mutex::new(State {
          buf: VecDeque::new(),
          cap,
          strategy,
          shutdown: false,
          takers: WaitList::default(),
          offerers: WaitList::default(),
        }),
        closed: Arc::new(Signal::default()),
      }),
    }
  }

  /// A bounded queue (`capacity` at least 1): [`offer`](Self::offer) suspends while it is full.
  pub fn bounded(capacity: usize) -> Self {
    Self::with(Strategy::Bounded, capacity.max(1))
  }

  /// An unbounded queue.
  pub fn unbounded() -> Self {
    Self::with(Strategy::Unbounded, usize::MAX)
  }

  /// A bounded queue that refuses the **incoming** item when full.
  pub fn dropping(capacity: usize) -> Self {
    Self::with(Strategy::Dropping, capacity.max(1))
  }

  /// A bounded queue that evicts the **oldest** item when full.
  pub fn sliding(capacity: usize) -> Self {
    Self::with(Strategy::Sliding, capacity.max(1))
  }

  /// The capacity (`None` for an unbounded queue).
  pub fn capacity(&self) -> Option<usize> {
    let st = lock(&self.inner.state);
    (st.strategy != Strategy::Unbounded).then_some(st.cap)
  }

  /// The offer as a plain future: `Err(item)` when the item was not stored (refused by a
  /// dropping queue or by a shutdown).
  pub(crate) fn offer_future(&self, item: A) -> OfferFuture<A> {
    OfferFuture {
      inner: Arc::clone(&self.inner),
      item: Some(item),
      id: None,
    }
  }

  /// One put without suspending: `Ok(stored)` (`false` when a dropping or shut-down queue
  /// refused it), `Err(item)` when a bounded queue is full.
  pub(crate) fn put_now(&self, item: A) -> Result<bool, A> {
    let mut ws = Vec::new();
    let put = lock(&self.inner.state).put(item, &mut ws);
    wake_all(ws);
    match put {
      Put::Stored => Ok(true),
      Put::Refused(_) => Ok(false),
      Put::Full(item) => Err(item),
    }
  }

  /// The head item without suspending (admitting a blocked offer into the freed slot):
  /// `Err(true)` when the queue is shut down and drained, `Err(false)` when it is empty.
  pub(crate) fn pop_now(&self) -> Result<A, bool> {
    let mut ws = Vec::new();
    let head = {
      let mut st = lock(&self.inner.state);
      match st.pop(&mut ws) {
        Some(v) => Ok(v),
        None => Err(st.shutdown),
      }
    };
    wake_all(ws);
    head
  }

  /// The take of one item as a plain future (no buffer, unlike [`take_future`](Self::take_future)).
  pub(crate) fn take_one_future(&self) -> TakeOneFuture<A> {
    TakeOneFuture {
      inner: Arc::clone(&self.inner),
      id: None,
    }
  }

  /// The take of `min..=max` items as a plain future.
  pub(crate) fn take_future(&self, min: usize, max: usize) -> TakeFuture<A> {
    TakeFuture {
      inner: Arc::clone(&self.inner),
      min,
      max,
      got: Vec::new(),
      id: None,
    }
  }

  /// Offers one item. `false` when it was not stored (a full dropping queue, or a shut-down
  /// queue). On a full bounded queue this suspends until a slot frees up; an interrupted offer
  /// stores nothing.
  ///
  /// An offer that completes at once (a free slot, a waiting taker, a refusal) is one
  /// synchronous step; only an offer to a full bounded queue becomes an async step.
  pub fn offer<E, R>(&self, item: A) -> Effect<bool, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let q = self.clone();
    suspend(move || match q.put_now(item) {
      Ok(stored) => succeed(stored),
      Err(item) => from_async(move || async move { Ok(q.offer_future(item).await.is_ok()) }),
    })
  }

  /// Offers one item without suspending: `false` when it was not stored (full, dropping, or
  /// shut down).
  pub fn try_offer<E, R>(&self, item: A) -> Effect<bool, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || {
      let mut ws = Vec::new();
      let stored = matches!(lock(&inner.state).put(item, &mut ws), Put::Stored);
      wake_all(ws);
      stored
    })
  }

  /// Offers every item in order (suspending on a full bounded queue); returns the items that were
  /// not stored.
  pub fn offer_all<E, R>(&self, items: impl IntoIterator<Item = A>) -> Effect<Vec<A>, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let items: Vec<A> = items.into_iter().collect();
    let q = self.clone();
    from_async(move || async move {
      let mut left = Vec::new();
      for item in items {
        if let Err(item) = q.offer_future(item).await {
          left.push(item);
        }
      }
      Ok(left)
    })
  }

  /// Offers every item in order without suspending; returns the items that were not stored (the
  /// v1 `offer_all`).
  pub fn try_offer_all<E, R>(&self, items: impl IntoIterator<Item = A>) -> Effect<Vec<A>, E, R> {
    let items: Vec<A> = items.into_iter().collect();
    let inner = Arc::clone(&self.inner);
    sync(move || {
      let mut ws = Vec::new();
      let left = {
        let mut st = lock(&inner.state);
        items
          .into_iter()
          .filter_map(|item| match st.put(item, &mut ws) {
            Put::Stored => None,
            Put::Refused(item) | Put::Full(item) => Some(item),
          })
          .collect()
      };
      wake_all(ws);
      left
    })
  }

  fn take_between_effect<R>(&self, min: usize, max: usize) -> Effect<Vec<A>, QueueError, R>
  where
    R: Send + Sync + 'static,
  {
    let q = self.clone();
    from_async(move || q.take_future(min, max))
  }

  /// Takes one item, suspending while the queue is empty. Fails with
  /// [`QueueError::Disconnected`] once the queue is shut down and drained.
  ///
  /// A take from a non-empty or shut-down queue is one synchronous step with no allocation;
  /// only a take from an empty open queue becomes an async step.
  pub fn take<R: Send + Sync + 'static>(&self) -> Effect<A, QueueError, R> {
    let q = self.clone();
    suspend(move || match q.pop_now() {
      Ok(v) => succeed(v),
      Err(true) => fail(QueueError::Disconnected),
      Err(false) => from_async(move || q.take_one_future()),
    })
  }

  /// Waits for at least one item, then takes every item currently buffered.
  pub fn take_all<R: Send + Sync + 'static>(&self) -> Effect<Vec<A>, QueueError, R> {
    self.take_between_effect(1, usize::MAX)
  }

  /// Waits for at least one item, then takes at most `n` (`n == 0` returns at once).
  pub fn take_up_to<R: Send + Sync + 'static>(&self, n: usize) -> Effect<Vec<A>, QueueError, R> {
    if n == 0 {
      return crate::kernel::succeed(Vec::new());
    }
    self.take_between_effect(1, n)
  }

  /// Takes exactly `n` items, suspending until they are available.
  pub fn take_n<R: Send + Sync + 'static>(&self, n: usize) -> Effect<Vec<A>, QueueError, R> {
    self.take_between_effect(n, n)
  }

  /// Takes between `min` and `max` items: suspends until `min` are available, then takes up to
  /// `max`. `min > max` and `max == 0` return an empty vector at once.
  pub fn take_between<R: Send + Sync + 'static>(
    &self,
    min: usize,
    max: usize,
  ) -> Effect<Vec<A>, QueueError, R> {
    if min > max || max == 0 {
      return crate::kernel::succeed(Vec::new());
    }
    self.take_between_effect(min, max)
  }

  /// Takes the head item if there is one, without suspending. Fails with
  /// [`QueueError::Disconnected`] when the queue is shut down and drained.
  pub fn poll<R>(&self) -> Effect<Option<A>, QueueError, R> {
    let inner = Arc::clone(&self.inner);
    let step = move || {
      let mut ws = Vec::new();
      let out = {
        let mut st = lock(&inner.state);
        match st.pop(&mut ws) {
          Some(v) => Ok(Some(v)),
          None if st.shutdown => Err(QueueError::Disconnected),
          None => Ok(None),
        }
      };
      wake_all(ws);
      out
    };
    crate::kernel::suspend(move || match step() {
      Ok(v) => crate::kernel::succeed(v),
      Err(e) => crate::kernel::fail(e),
    })
  }

  /// The number of buffered items.
  pub fn size<E, R>(&self) -> Effect<usize, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || lock(&inner.state).buf.len())
  }

  /// `true` when no item is buffered.
  pub fn is_empty<E, R>(&self) -> Effect<bool, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || lock(&inner.state).buf.is_empty())
  }

  /// `true` when the buffer is at capacity (never for an unbounded queue).
  pub fn is_full<E, R>(&self) -> Effect<bool, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || {
      let st = lock(&inner.state);
      st.strategy != Strategy::Unbounded && st.buf.len() >= st.cap
    })
  }

  /// Shuts the queue down: suspended offers return `false`, suspended takers fail with
  /// [`QueueError::Disconnected`], buffered items stay available to takers. Idempotent.
  pub fn shutdown<E, R>(&self) -> Effect<(), E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || shutdown_now(&inner))
  }

  /// `true` once [`shutdown`](Self::shutdown) has run.
  pub fn is_shutdown<E, R>(&self) -> Effect<bool, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || lock(&inner.state).shutdown)
  }

  /// Suspends until the queue is shut down.
  pub fn await_shutdown<E, R>(&self) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let closed = Arc::clone(&self.inner.closed);
    from_async(move || async move {
      closed.wait().await;
      Ok(())
    })
  }

  /// [`shutdown`](Self::shutdown) outside an effect (finalizers and other primitives).
  pub(crate) fn shutdown_sync(&self) {
    shutdown_now(&self.inner);
  }

  /// [`is_full`](Self::is_full) outside an effect.
  pub(crate) fn is_full_now(&self) -> bool {
    let st = lock(&self.inner.state);
    st.strategy != Strategy::Unbounded && st.buf.len() >= st.cap
  }

  /// [`size`](Self::size) outside an effect.
  pub(crate) fn size_now(&self) -> usize {
    lock(&self.inner.state).buf.len()
  }
}

fn shutdown_now<A>(inner: &Inner<A>) {
  let ws = {
    let mut st = lock(&inner.state);
    if st.shutdown {
      return;
    }
    st.shutdown = true;
    let mut ws: Vec<Waker> = st.takers.drain().into_iter().map(|(_, (), w)| w).collect();
    for (id, item, w) in st.offerers.drain() {
      st.offerers.grant(id, Some(item));
      ws.push(w);
    }
    ws
  };
  wake_all(ws);
  inner.closed.fire();
}

/// The suspended offer of one item.
pub(crate) struct OfferFuture<A> {
  inner: Arc<Inner<A>>,
  item: Option<A>,
  id: Option<u64>,
}

impl<A> Unpin for OfferFuture<A> {}

impl<A> Future for OfferFuture<A> {
  type Output = Result<(), A>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), A>> {
    let this = &mut *self;
    let mut ws = Vec::new();
    let out = {
      let mut st = lock(&this.inner.state);
      match (this.id, this.item.take()) {
        (Some(id), _) => match st.offerers.take_grant(id) {
          Some(None) => {
            this.id = None;
            Poll::Ready(Ok(()))
          }
          Some(Some(item)) => {
            this.id = None;
            Poll::Ready(Err(item))
          }
          None => {
            st.offerers.refresh(id, cx.waker());
            Poll::Pending
          }
        },
        (None, Some(item)) => match st.put(item, &mut ws) {
          Put::Stored => Poll::Ready(Ok(())),
          Put::Refused(item) => Poll::Ready(Err(item)),
          Put::Full(item) => {
            this.id = Some(st.offerers.push(item, cx.waker()));
            Poll::Pending
          }
        },
        (None, None) => panic!("id_effect: queue offer polled after completion"),
      }
    };
    wake_all(ws);
    out
  }
}

impl<A> Drop for OfferFuture<A> {
  fn drop(&mut self) {
    if let Some(id) = self.id.take() {
      let mut st = lock(&self.inner.state);
      // A grant means the item was stored (or refused by a shutdown): nothing to undo.
      if st.offerers.take_grant(id).is_none() {
        st.offerers.remove(id);
      }
    }
  }
}

/// The suspended take of one item ([`Queue::take`] on an empty queue).
pub(crate) struct TakeOneFuture<A> {
  inner: Arc<Inner<A>>,
  id: Option<u64>,
}

impl<A> Unpin for TakeOneFuture<A> {}

impl<A> Future for TakeOneFuture<A> {
  type Output = Result<A, QueueError>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    let mut ws = Vec::new();
    let out = {
      let mut st = lock(&this.inner.state);
      if let Some(id) = this.id
        && let Some(v) = st.takers.take_grant(id)
      {
        this.id = None;
        Poll::Ready(Ok(v))
      } else if let Some(v) = st.pop(&mut ws) {
        if let Some(id) = this.id.take() {
          st.takers.remove(id);
        }
        Poll::Ready(Ok(v))
      } else if st.shutdown {
        if let Some(id) = this.id.take() {
          st.takers.remove(id);
        }
        Poll::Ready(Err(QueueError::Disconnected))
      } else {
        match this.id {
          Some(id) if st.takers.refresh(id, cx.waker()) => {}
          _ => this.id = Some(st.takers.push((), cx.waker())),
        }
        Poll::Pending
      }
    };
    wake_all(ws);
    out
  }
}

impl<A> Drop for TakeOneFuture<A> {
  fn drop(&mut self) {
    let Some(id) = self.id.take() else {
      return;
    };
    let mut ws = Vec::new();
    {
      let mut st = lock(&self.inner.state);
      match st.takers.take_grant(id) {
        // Handed over but never observed (interrupted): it goes back to the queue.
        Some(v) => st.put_back(v, &mut ws),
        None => {
          st.takers.remove(id);
        }
      }
    }
    wake_all(ws);
  }
}

/// The suspended take of `min..=max` items.
pub(crate) struct TakeFuture<A> {
  inner: Arc<Inner<A>>,
  min: usize,
  max: usize,
  got: Vec<A>,
  id: Option<u64>,
}

impl<A> Unpin for TakeFuture<A> {}

impl<A> Future for TakeFuture<A> {
  type Output = Result<Vec<A>, QueueError>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    let mut ws = Vec::new();
    let out = {
      let mut st = lock(&this.inner.state);
      if let Some(id) = this.id
        && let Some(v) = st.takers.take_grant(id)
      {
        this.id = None;
        this.got.push(v);
      }
      while this.got.len() < this.max {
        match st.pop(&mut ws) {
          Some(v) => this.got.push(v),
          None => break,
        }
      }
      if this.got.len() >= this.min {
        if let Some(id) = this.id.take() {
          st.takers.remove(id);
        }
        Poll::Ready(Ok(std::mem::take(&mut this.got)))
      } else if st.shutdown {
        if let Some(id) = this.id.take() {
          st.takers.remove(id);
        }
        for v in std::mem::take(&mut this.got).into_iter().rev() {
          st.put_back(v, &mut ws);
        }
        Poll::Ready(Err(QueueError::Disconnected))
      } else {
        match this.id {
          Some(id) if st.takers.refresh(id, cx.waker()) => {}
          _ => this.id = Some(st.takers.push((), cx.waker())),
        }
        Poll::Pending
      }
    };
    wake_all(ws);
    out
  }
}

impl<A> Drop for TakeFuture<A> {
  fn drop(&mut self) {
    let id = self.id.take();
    if id.is_none() && self.got.is_empty() {
      return;
    }
    let mut ws = Vec::new();
    {
      let mut st = lock(&self.inner.state);
      if let Some(id) = id {
        match st.takers.take_grant(id) {
          // Handed over but never observed (interrupted): it goes back to the queue.
          Some(v) => self.got.push(v),
          None => {
            st.takers.remove(id);
          }
        }
      }
      for v in std::mem::take(&mut self.got).into_iter().rev() {
        st.put_back(v, &mut ws);
      }
    }
    wake_all(ws);
  }
}
