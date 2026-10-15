//! Producer-driven streams: [`stream_from_channel`] returns a [`Stream`] and a
//! [`StreamSender`] that feeds it through a [`Queue`] of chunks, with a [`BackpressurePolicy`]
//! for a full queue.
//!
//! The end of the stream is a queue shutdown (buffered chunks stay readable), and a failure is
//! stored in a take-once slot before the shutdown, so the consumer sees every chunk sent before
//! it and then the error. Neither ever waits for space or spins: [`StreamSender::fail`] and
//! [`end_stream`] return at once (the 0.6 `fail` busy-looped a blocking offer plus a thread
//! yield while the queue was full). Dropping the last sender ends the stream too.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::stream::{FailSlot, Stream, lock, queue_source};
use crate::coordination::Queue;
use crate::failure::Never;
use crate::kernel::{Effect, fail, succeed, sync};
use crate::streaming::chunk::Chunk;

/// How [`stream_from_channel_with_policy`] behaves when its queue of chunks is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackpressurePolicy {
  /// The producer suspends until there is room.
  BoundedBlock,
  /// The newly offered chunk is dropped.
  DropNewest,
  /// The oldest queued chunk is evicted to make room.
  DropOldest,
  /// The send fails with [`StreamChannelFull`].
  Fail,
}

/// The action chosen for one enqueue attempt, given the policy and the fill level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackpressureDecision {
  /// Store the item.
  Enqueue,
  /// The producer waits for room.
  Block,
  /// Discard the new item.
  DropNewest,
  /// Evict the oldest item, then store the new one.
  DropOldest,
  /// Report the full queue to the producer.
  Fail,
}

/// Maps `(policy, queue_len, capacity)` to a [`BackpressureDecision`] (a capacity below 1
/// counts as 1).
pub fn backpressure_decision(
  policy: BackpressurePolicy,
  queue_len: usize,
  capacity: usize,
) -> BackpressureDecision {
  if queue_len < capacity.max(1) {
    return BackpressureDecision::Enqueue;
  }
  match policy {
    BackpressurePolicy::BoundedBlock => BackpressureDecision::Block,
    BackpressurePolicy::DropNewest => BackpressureDecision::DropNewest,
    BackpressurePolicy::DropOldest => BackpressureDecision::DropOldest,
    BackpressurePolicy::Fail => BackpressureDecision::Fail,
  }
}

/// [`send_chunk`] could not enqueue: the queue is full (or the stream has ended) under
/// [`BackpressurePolicy::Fail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamChannelFull;

impl fmt::Display for StreamChannelFull {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "stream channel is full (backpressure policy Fail)")
  }
}

impl std::error::Error for StreamChannelFull {}

struct SenderInner<A: Send + 'static, E> {
  queue: Queue<Chunk<A>>,
  failure: FailSlot<E>,
  policy: BackpressurePolicy,
  ended: AtomicBool,
}

impl<A: Send + 'static, E> SenderInner<A, E> {
  /// Ends the stream; `false` when it had already ended.
  fn end(&self) -> bool {
    let first = !self.ended.swap(true, Ordering::AcqRel);
    if first {
      self.queue.shutdown_sync();
    }
    first
  }
}

impl<A: Send + 'static, E> Drop for SenderInner<A, E> {
  fn drop(&mut self) {
    self.end();
  }
}

/// The producer handle of a [`stream_from_channel`] stream. Cloning yields another handle; the
/// stream ends when [`end_stream`] runs or the last handle is dropped.
pub struct StreamSender<A: Send + 'static, E> {
  inner: Arc<SenderInner<A, E>>,
}

impl<A: Send + 'static, E> Clone for StreamSender<A, E> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<A: Send + 'static, E> fmt::Debug for StreamSender<A, E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("StreamSender")
      .field("policy", &self.inner.policy)
      .field("ended", &self.inner.ended.load(Ordering::Acquire))
      .finish()
  }
}

impl<A, E> StreamSender<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  /// The policy for a full queue.
  pub fn policy(&self) -> BackpressurePolicy {
    self.inner.policy
  }

  /// Ends the stream with `error`: the consumer reads every chunk sent before it, then fails
  /// with `error`. Returns `false` (and drops `error`) when the stream has already ended or
  /// failed. Never waits.
  pub fn fail(&self, error: E) -> bool {
    if self.inner.ended.load(Ordering::Acquire) {
      return false;
    }
    *lock(&self.inner.failure) = Some(error);
    if self.inner.end() {
      true
    } else {
      // Lost a race with another end: the stream ended without this error.
      lock(&self.inner.failure).take();
      false
    }
  }

  /// [`send_chunk`] as a method.
  pub fn send<R: Send + Sync + 'static>(
    &self,
    chunk: Chunk<A>,
  ) -> Effect<(), StreamChannelFull, R> {
    send_chunk(self, chunk)
  }
}

/// A stream fed by a [`StreamSender`] through a queue of at most `capacity` (at least 1) chunks,
/// with an explicit [`BackpressurePolicy`].
pub fn stream_from_channel_with_policy<A, E, R>(
  capacity: usize,
  policy: BackpressurePolicy,
) -> (Stream<A, E, R>, StreamSender<A, E>)
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let cap = capacity.max(1);
  let queue = match policy {
    BackpressurePolicy::BoundedBlock | BackpressurePolicy::Fail => Queue::bounded(cap),
    BackpressurePolicy::DropNewest => Queue::dropping(cap),
    BackpressurePolicy::DropOldest => Queue::sliding(cap),
  };
  let failure: FailSlot<E> = Arc::new(Mutex::new(None));
  let stream = queue_source(
    queue.clone(),
    Arc::clone(&failure),
    VecDeque::new(),
    flatten_chunks,
  );
  let sender = StreamSender {
    inner: Arc::new(SenderInner {
      queue,
      failure,
      policy,
      ended: AtomicBool::new(false),
    }),
  };
  (stream, sender)
}

fn flatten_chunks<A>(chunks: Vec<Chunk<A>>) -> Vec<A> {
  chunks.into_iter().flat_map(Chunk::into_vec).collect()
}

/// [`stream_from_channel_with_policy`] with [`BackpressurePolicy::BoundedBlock`].
///
/// ```
/// use id_effect::{Chunk, end_stream, send_chunk, stream_from_channel};
/// use id_effect::Never;
///
/// let (stream, sender) = stream_from_channel::<u8, Never, _>(4);
/// let eff = send_chunk(&sender, Chunk::from_vec(vec![1, 2]))
///   .then(send_chunk(&sender, Chunk::singleton(3)))
///   .then(end_stream(sender).widen())
///   .then(stream.run_collect().map_err(|n| match n {}));
/// assert_eq!(eff.block_on().into_result().ok(), Some(vec![1, 2, 3]));
/// ```
pub fn stream_from_channel<A, E, R>(capacity: usize) -> (Stream<A, E, R>, StreamSender<A, E>)
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  stream_from_channel_with_policy(capacity, BackpressurePolicy::BoundedBlock)
}

/// Enqueues `chunk` for the consumer, per the sender's policy: suspends while the queue is full
/// ([`BoundedBlock`](BackpressurePolicy::BoundedBlock)), drops a chunk
/// ([`DropNewest`](BackpressurePolicy::DropNewest) /
/// [`DropOldest`](BackpressurePolicy::DropOldest)) or fails with [`StreamChannelFull`]
/// ([`Fail`](BackpressurePolicy::Fail)). After the stream has ended the chunk is discarded (an
/// error only under `Fail`). An empty chunk is a no-op.
pub fn send_chunk<A, E, R>(
  sender: &StreamSender<A, E>,
  chunk: Chunk<A>,
) -> Effect<(), StreamChannelFull, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  if chunk.is_empty() {
    return succeed(());
  }
  // The effect keeps the sender alive until the offer completes, so dropping the last handle
  // right after building a send does not end the stream under it.
  let inner = Arc::clone(&sender.inner);
  let queue = inner.queue.clone();
  match inner.policy {
    BackpressurePolicy::BoundedBlock => queue.offer(chunk).map(move |_| drop(inner)),
    BackpressurePolicy::DropNewest | BackpressurePolicy::DropOldest => {
      queue.try_offer(chunk).map(move |_| drop(inner))
    }
    BackpressurePolicy::Fail => queue.try_offer(chunk).flat_map(move |stored| {
      drop(inner);
      if stored {
        succeed(())
      } else {
        fail(StreamChannelFull)
      }
    }),
  }
}

/// Ends the stream: the consumer reads what is buffered, then the stream ends. Never waits.
pub fn end_stream<A, E, R>(sender: StreamSender<A, E>) -> Effect<(), Never, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  sync(move || {
    sender.inner.end();
  })
}
