//! The stream-, sink- and fold-backed channel variants of the 0.6 `coordination::Channel`,
//! deferred by `c-coordination` to the streaming leaf and built here on top of
//! [`next::coordination::{Queue, Channel}`](crate::next::coordination):
//!
//! - [`Channel::to_stream`] / [`Stream::from_channel`]: read a queue-backed [`Channel`] as a
//!   stream (one `read` per pull, so nothing is read ahead); [`Channel::to_sink`] writes a stream
//!   into one.
//! - [`StreamChannel`] (0.6 `Channel::from_stream`): a read-only channel over a stream.
//! - [`FoldChannel`] (0.6 `Channel::from_fold` / `from_sink`): every write folds into an
//!   accumulator and publishes a snapshot for readers.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::sink::Sink;
use super::stream::{Stream, lock};
use crate::failure::Never;
use crate::next::capability::HNil;
use crate::next::coordination::{Channel, Queue, QueueError};
use crate::next::kernel::{Effect, succeed, suspend, sync};

type Emit<A> = Arc<dyn Fn(A) -> Vec<A> + Send + Sync>;

impl<Out: Send + 'static, In: Send + 'static> Channel<Out, In> {
  /// The channel's output as a stream: each pull performs exactly one
  /// [`read`](Channel::read), and the stream ends once the channel is shut down and drained.
  pub fn to_stream<E, R>(&self) -> Stream<Out, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    Stream::from_channel(self)
  }

  /// A sink that [`write`](Channel::write)s every element of a stream into this channel.
  pub fn to_sink<E, R>(&self) -> Sink<(), In, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let ch = self.clone();
    Sink::new(move |stream: Stream<In, E, R>| {
      let ch = ch.clone();
      stream.run_for_each_effect(move |x| ch.write(x))
    })
  }
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// The output of `ch` as a stream (see [`Channel::to_stream`]).
  pub fn from_channel<In: Send + 'static>(ch: &Channel<A, In>) -> Self {
    channel_reader(ch.clone())
  }

  /// This stream as a read-only [`StreamChannel`].
  pub fn into_channel(self) -> StreamChannel<A, E, R> {
    StreamChannel::new(self)
  }
}

fn channel_reader<A, In, E, R>(ch: Channel<A, In>) -> Stream<A, E, R>
where
  A: Send + 'static,
  In: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Stream::from_step(move |_| {
    let read = ch.read::<E, R>();
    read.map(move |next| next.map(|a| (vec![a], channel_reader(ch))))
  })
}

// -----------------------------------------------------------------------------------------------
// StreamChannel
// -----------------------------------------------------------------------------------------------

/// A read-only channel over a [`Stream`] (0.6 `Channel::from_stream`): [`read`](Self::read)
/// returns the stream's elements one at a time, `None` at its end, and fails with the stream's
/// error. A pulled chunk is buffered for the following reads. Cloning yields another handle on
/// the same stream; concurrent reads take turns (the stream is parked in a one-slot [`Queue`]
/// between reads), and a read that fails or is interrupted while pulling ends the stream for
/// every handle.
pub struct StreamChannel<A, E = Never, R = HNil> {
  /// The stream before the first read (the slot queue cannot be filled outside an effect).
  initial: Arc<Mutex<Option<Stream<A, E, R>>>>,
  slot: Queue<Stream<A, E, R>>,
  pending: Arc<Mutex<VecDeque<A>>>,
  emit: Emit<A>,
}

impl<A, E, R> Clone for StreamChannel<A, E, R> {
  fn clone(&self) -> Self {
    Self {
      initial: Arc::clone(&self.initial),
      slot: self.slot.clone(),
      pending: Arc::clone(&self.pending),
      emit: Arc::clone(&self.emit),
    }
  }
}

impl<A, E, R> std::fmt::Debug for StreamChannel<A, E, R> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("StreamChannel")
      .field("pending", &lock(&self.pending).len())
      .finish_non_exhaustive()
  }
}

impl<A, E, R> StreamChannel<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// A channel reading `stream`.
  pub fn new(stream: Stream<A, E, R>) -> Self {
    Self {
      initial: Arc::new(Mutex::new(Some(stream))),
      slot: Queue::bounded(1),
      pending: Arc::new(Mutex::new(VecDeque::new())),
      emit: Arc::new(|a| vec![a]),
    }
  }

  /// Reads the next element (`None` once the stream has ended).
  pub fn read(&self) -> Effect<Option<A>, E, R> {
    let ch = self.clone();
    suspend(move || {
      if let Some(a) = lock(&ch.pending).pop_front() {
        return succeed(Some(a));
      }
      let next = ch.clone();
      let first = lock(&ch.initial).take();
      match first {
        Some(stream) => next.read_from(stream),
        None => ch
          .slot
          .take::<R>()
          .catch(|_: QueueError| succeed(Stream::empty()))
          .flat_map(move |stream| next.read_from(stream)),
      }
    })
  }

  /// Pulls one chunk from `stream` (taken out of the slot) and puts the rest back; a failed or
  /// interrupted pull puts an empty stream back instead.
  fn read_from(self, stream: Stream<A, E, R>) -> Effect<Option<A>, E, R> {
    let returned = Arc::new(AtomicBool::new(false));
    let fin = {
      let (slot, returned) = (self.slot.clone(), Arc::clone(&returned));
      suspend(move || {
        if returned.load(Ordering::Acquire) {
          succeed(())
        } else {
          slot.try_offer(Stream::empty()).map(|_| ())
        }
      })
    };
    let ch = self;
    stream
      .next_chunk()
      .flat_map(move |next| {
        let rest = match next {
          None => {
            returned.store(true, Ordering::Release);
            return ch.slot.try_offer(Stream::empty()).map(|_| None);
          }
          Some((chunk, rest)) => {
            let out: VecDeque<A> = chunk.into_vec().into_iter().flat_map(&*ch.emit).collect();
            lock(&ch.pending).extend(out);
            rest
          }
        };
        returned.store(true, Ordering::Release);
        ch.slot.try_offer(rest).flat_map(move |_| {
          let first = lock(&ch.pending).pop_front();
          match first {
            Some(a) => succeed(Some(a)),
            // Every element of the chunk expanded to nothing: read on.
            None => ch.read(),
          }
        })
      })
      .ensuring(fin)
  }

  /// The channel's elements as a stream (one [`read`](Self::read) per pull).
  pub fn to_stream(&self) -> Stream<A, E, R> {
    let ch = self.clone();
    Stream::from_step(move |_| {
      let read = ch.read();
      read.map(move |next| next.map(|a| (vec![a], ch.to_stream())))
    })
  }

  /// Maps each element on the read side.
  #[must_use]
  pub fn map_out(self, f: impl Fn(A) -> A + Send + Sync + 'static) -> Self {
    let emit = self.emit;
    Self {
      emit: Arc::new(move |a| emit(a).into_iter().map(&f).collect()),
      ..self
    }
  }

  /// Expands each element on the read side into zero or more elements (an empty expansion is
  /// skipped).
  #[must_use]
  pub fn flat_map_out(self, f: impl Fn(A) -> Vec<A> + Send + Sync + 'static) -> Self {
    let emit = self.emit;
    Self {
      emit: Arc::new(move |a| emit(a).into_iter().flat_map(&f).collect()),
      ..self
    }
  }
}

// -----------------------------------------------------------------------------------------------
// FoldChannel
// -----------------------------------------------------------------------------------------------

type FoldFn<Acc, In> = Arc<dyn Fn(Acc, In) -> Acc + Send + Sync>;

/// A fold-backed channel (0.6 `Channel::from_fold` / `Channel::from_sink`): each
/// [`write`](Self::write) folds the value into an accumulator and publishes the new
/// accumulator as a snapshot; [`read`](Self::read) returns the snapshots in order and
/// [`fold_state`](Self::fold_state) the current accumulator. Cloning yields another handle on
/// the same accumulator.
///
/// ```
/// use id_effect::next::{FoldChannel, Stream};
/// use id_effect::Never;
///
/// let sum = FoldChannel::from_fold(0_u32, |acc, x: u32| acc + x);
/// let total = sum.consume_stream(Stream::<u32, Never>::from_iterable([1, 2, 3]));
/// assert_eq!(total.block_on().into_result().ok(), Some(6));
/// ```
pub struct FoldChannel<Acc, In> {
  acc: Arc<Mutex<Acc>>,
  step: FoldFn<Acc, In>,
  snapshots: Queue<Acc>,
  pending: Arc<Mutex<VecDeque<Acc>>>,
  emit: Emit<Acc>,
}

impl<Acc, In> Clone for FoldChannel<Acc, In> {
  fn clone(&self) -> Self {
    Self {
      acc: Arc::clone(&self.acc),
      step: Arc::clone(&self.step),
      snapshots: self.snapshots.clone(),
      pending: Arc::clone(&self.pending),
      emit: Arc::clone(&self.emit),
    }
  }
}

impl<Acc, In> std::fmt::Debug for FoldChannel<Acc, In> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FoldChannel")
      .field("snapshots", &self.snapshots)
      .finish_non_exhaustive()
  }
}

impl<Acc, In> FoldChannel<Acc, In>
where
  Acc: Clone + Send + 'static,
  In: Send + 'static,
{
  /// A channel folding every write into `init` with `step`.
  pub fn from_fold(init: Acc, step: impl Fn(Acc, In) -> Acc + Send + Sync + 'static) -> Self {
    Self::with_step(init, Arc::new(step))
  }

  fn with_step(init: Acc, step: FoldFn<Acc, In>) -> Self {
    Self {
      acc: Arc::new(Mutex::new(init)),
      step,
      snapshots: Queue::unbounded(),
      pending: Arc::new(Mutex::new(VecDeque::new())),
      emit: Arc::new(|a| vec![a]),
    }
  }

  /// The fold of a [`Sink::fold_left`] sink as a writable channel.
  ///
  /// # Panics
  ///
  /// If `sink` is not a fold ([`Sink::fold_left`] / [`Sink::from_fold`] and the sinks built on
  /// them). Other sinks can feed a queue with [`Sink::to_queue`] and a [`Channel`].
  pub fn from_sink<E, R>(sink: Sink<Acc, In, E, R>) -> Self
  where
    Acc: Sync,
  {
    let fold = sink
      .fold
      .expect("FoldChannel::from_sink requires a Sink::fold_left / Sink::from_fold sink");
    Self::with_step(fold.init.clone(), Arc::clone(&fold.step))
  }

  /// Folds `value` into the accumulator and publishes the new accumulator for readers.
  pub fn write<E, R>(&self, value: In) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let ch = self.clone();
    suspend(move || {
      let next = {
        let mut acc = lock(&ch.acc);
        let next = (ch.step)(acc.clone(), value);
        *acc = next.clone();
        next
      };
      ch.snapshots.try_offer(next).map(|_| ())
    })
  }

  /// The current accumulator.
  pub fn fold_state<E, R>(&self) -> Effect<Acc, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let acc = Arc::clone(&self.acc);
    sync(move || lock(&acc).clone())
  }

  /// The next snapshot, in write order (suspending until a write); `None` once the channel is
  /// [shut down](Self::shutdown) and every snapshot has been read.
  pub fn read<E, R>(&self) -> Effect<Option<Acc>, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let ch = self.clone();
    suspend(move || {
      if let Some(a) = lock(&ch.pending).pop_front() {
        return succeed(Some(a));
      }
      let next = ch.clone();
      ch.snapshots
        .take::<R>()
        .map(Some)
        .catch(|_: QueueError| succeed(None))
        .flat_map(move |snap| match snap {
          None => succeed(None),
          Some(s) => {
            let mut out: VecDeque<Acc> = (next.emit)(s).into();
            match out.pop_front() {
              Some(first) => {
                lock(&next.pending).extend(out);
                succeed(Some(first))
              }
              None => next.read(),
            }
          }
        })
    })
  }

  /// Folds every element of `stream` into the channel (as [`write`](Self::write)s), then
  /// returns the final accumulator. A stream failure ends the drain with that error.
  pub fn consume_stream<E, R>(&self, stream: Stream<In, E, R>) -> Effect<Acc, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let (w, s) = (self.clone(), self.clone());
    stream
      .run_for_each_effect(move |x| w.write(x))
      .flat_map(move |()| s.fold_state())
  }

  /// Ends the snapshot feed: snapshots already published stay readable, then reads return
  /// `None`. Later writes still fold but publish nothing.
  pub fn shutdown<E, R>(&self) -> Effect<(), E, R> {
    self.snapshots.shutdown()
  }

  /// Maps each snapshot on the read side.
  #[must_use]
  pub fn map_out(self, f: impl Fn(Acc) -> Acc + Send + Sync + 'static) -> Self {
    let emit = self.emit;
    Self {
      emit: Arc::new(move |a| emit(a).into_iter().map(&f).collect()),
      ..self
    }
  }

  /// Expands each snapshot on the read side into zero or more values (an empty expansion is
  /// skipped).
  #[must_use]
  pub fn flat_map_out(self, f: impl Fn(Acc) -> Vec<Acc> + Send + Sync + 'static) -> Self {
    let emit = self.emit;
    Self {
      emit: Arc::new(move |a| emit(a).into_iter().flat_map(&f).collect()),
      ..self
    }
  }
}
