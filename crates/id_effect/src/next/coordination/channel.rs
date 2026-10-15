//! [`Channel`]: a queue-backed duplex pipeline. Ported from the queue-backed half of the v1
//! `coordination::channel` (`Channel::from_queue_and_map` / `QueueChannel`).
//!
//! [`Channel::write`] maps each input through the write side (`In → Out`, see
//! [`map_in`](Channel::map_in)) into a shared [`Queue`]; [`Channel::read`] takes the next wire
//! element, expands it through the read side ([`map_out`](Channel::map_out),
//! [`flat_map_out`](Channel::flat_map_out)) and returns it, or `None` once the queue is shut down
//! and drained. The v1 stream-, sink- and fold-backed variants depend on `Stream`/`Sink`, which
//! are not on the next kernel yet; they move with the streaming leaf (`m-streams`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::queue::{Queue, QueueError};
use super::waiters::lock;
use crate::next::kernel::{Effect, from_async};

type Ingest<In, Out> = Arc<dyn Fn(In) -> Vec<Out> + Send + Sync>;
type Emit<Out> = Arc<dyn Fn(Out) -> Vec<Out> + Send + Sync>;

/// A duplex pipeline over a [`Queue`]: writes of `In` come out of reads as `Out`. Cloning yields
/// another handle on the same queue and read buffer.
///
/// ```
/// use id_effect::next::Channel;
/// use id_effect::runtime::Never;
///
/// let ch = Channel::<u32>::duplex_unbounded().map_in(|s: &str| s.len() as u32);
/// let eff = ch.write::<Never, _>("abc").then(ch.read());
/// assert_eq!(eff.block_on().into_result().ok(), Some(Some(3)));
/// ```
pub struct Channel<Out, In = Out> {
  queue: Queue<Out>,
  ingest: Ingest<In, Out>,
  emit: Emit<Out>,
  pending: Arc<Mutex<VecDeque<Out>>>,
}

impl<Out, In> Clone for Channel<Out, In> {
  fn clone(&self) -> Self {
    Self {
      queue: self.queue.clone(),
      ingest: Arc::clone(&self.ingest),
      emit: Arc::clone(&self.emit),
      pending: Arc::clone(&self.pending),
    }
  }
}

impl<Out, In> std::fmt::Debug for Channel<Out, In> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Channel")
      .field("queue", &self.queue)
      .field("pending", &lock(&self.pending).len())
      .finish()
  }
}

impl<T: Send + 'static> Channel<T, T> {
  /// An unbounded duplex channel whose input and output types are the same.
  pub fn duplex_unbounded() -> Self {
    Self::from_queue_and_map(Queue::unbounded(), |t| t)
  }

  /// A bounded duplex channel: [`write`](Channel::write) suspends while `capacity` elements are
  /// unread.
  pub fn duplex_bounded(capacity: usize) -> Self {
    Self::from_queue_and_map(Queue::bounded(capacity), |t| t)
  }
}

impl<Out: Send + 'static, In: Send + 'static> Channel<Out, In> {
  /// A channel over `queue`; each written `In` is mapped with `map_in` before it is offered.
  pub fn from_queue_and_map(
    queue: Queue<Out>,
    map_in: impl Fn(In) -> Out + Send + Sync + 'static,
  ) -> Self {
    Self {
      queue,
      ingest: Arc::new(move |i| vec![map_in(i)]),
      emit: Arc::new(|o| vec![o]),
      pending: Arc::new(Mutex::new(VecDeque::new())),
    }
  }

  /// The backing queue.
  pub fn queue(&self) -> &Queue<Out> {
    &self.queue
  }

  /// Writes one element (suspending while a bounded queue is full). After
  /// [`shutdown`](Self::shutdown) the element is dropped.
  pub fn write<E, R>(&self, value: In) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let queue = self.queue.clone();
    let ingest = Arc::clone(&self.ingest);
    from_async(move || async move {
      for out in ingest(value) {
        let _ = queue.offer_future(out).await;
      }
      Ok(())
    })
  }

  /// Reads the next element, suspending while none is available; `None` once the queue is shut
  /// down and drained.
  pub fn read<E, R>(&self) -> Effect<Option<Out>, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let ch = self.clone();
    from_async(move || async move {
      loop {
        if let Some(v) = lock(&ch.pending).pop_front() {
          return Ok(Some(v));
        }
        match ch.queue.take_future(1, 1).await {
          Ok(mut wire) => {
            let Some(w) = wire.pop() else { continue };
            let mut out: VecDeque<Out> = (ch.emit)(w).into();
            let Some(first) = out.pop_front() else {
              continue;
            };
            lock(&ch.pending).extend(out);
            return Ok(Some(first));
          }
          Err(QueueError::Disconnected) => return Ok(lock(&ch.pending).pop_front()),
        }
      }
    })
  }

  /// Shuts the backing queue down; elements already written stay readable.
  pub fn shutdown<E, R>(&self) -> Effect<(), E, R> {
    self.queue.shutdown()
  }

  /// Maps `In2` into this channel's input before the write side.
  #[must_use]
  pub fn map_in<In2: Send + 'static>(
    self,
    f: impl Fn(In2) -> In + Send + Sync + 'static,
  ) -> Channel<Out, In2> {
    let ingest = self.ingest;
    Channel {
      queue: self.queue,
      ingest: Arc::new(move |i| ingest(f(i))),
      emit: self.emit,
      pending: self.pending,
    }
  }

  /// Maps each element on the read side.
  #[must_use]
  pub fn map_out(self, f: impl Fn(Out) -> Out + Send + Sync + 'static) -> Self {
    let emit = self.emit;
    Self {
      emit: Arc::new(move |w| emit(w).into_iter().map(&f).collect()),
      ..self
    }
  }

  /// Expands each element on the read side into zero or more elements (buffered for the
  /// following reads; an empty expansion is skipped).
  #[must_use]
  pub fn flat_map_out(self, f: impl Fn(Out) -> Vec<Out> + Send + Sync + 'static) -> Self {
    let emit = self.emit;
    Self {
      emit: Arc::new(move |w| emit(w).into_iter().flat_map(&f).collect()),
      ..self
    }
  }

  /// Feeds this channel's output into `other`'s input: writes go through this channel's write
  /// and read sides, then `other`'s write side, into `other`'s queue.
  #[must_use]
  pub fn pipe<Out2: Send + 'static>(self, other: Channel<Out2, Out>) -> Channel<Out2, In> {
    let (ingest, emit, next) = (self.ingest, self.emit, other.ingest);
    Channel {
      queue: other.queue,
      ingest: Arc::new(move |i| {
        ingest(i)
          .into_iter()
          .flat_map(&*emit)
          .flat_map(&*next)
          .collect()
      }),
      emit: other.emit,
      pending: other.pending,
    }
  }
}
