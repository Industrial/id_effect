//! [`Mailbox`] — a process's bounded, single-consumer message queue.
//!
//! Producers enqueue **synchronously** ([`Mailbox::offer_now`]): exit signals and monitor downs
//! are delivered from plain code (link fan-out, monitor sinks, transports), so enqueueing must not
//! need an effect run edge. The lock is a `std` mutex whose critical sections never await, so an
//! enqueue never waits for another process (0.6 interpreted `Queue::offer` in place for this,
//! the `run_mailbox_op` workaround that 0.7 deletes).
//!
//! The one consumer is the process loop: [`Mailbox::take`] suspends on a real waker while the
//! mailbox is empty. An item leaves the buffer only in the poll that returns it, so an
//! interrupted take loses nothing.
//!
//! ## Bound (M10)
//!
//! Every mailbox holds at most [`capacity`](Mailbox::capacity) user messages
//! ([`DEFAULT_MAILBOX_CAPACITY`] unless set with [`Mailbox::with_capacity`]): a full mailbox
//! rejects [`try_offer_now`](Mailbox::try_offer_now) with [`OfferError::Full`] instead of growing
//! without limit, so a fast or hostile sender cannot exhaust the node's memory. System messages
//! (exit signals, monitor downs) enter through [`offer_system_now`](Mailbox::offer_system_now),
//! which bypasses the bound: a supervisor must learn that its child died even when the child's
//! peers flood it. There is no unbounded mailbox.

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use id_effect::{Effect, from_async, sync};

use crate::msg::ReceiveError;

/// A take on a mailbox that is closed and drained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailboxClosed;

impl fmt::Display for MailboxClosed {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("mailbox closed")
  }
}

impl std::error::Error for MailboxClosed {}

/// User-message capacity of a mailbox built with [`Mailbox::new`] (and of every process spawned
/// without [`SpawnOptions`](crate::SpawnOptions)).
pub const DEFAULT_MAILBOX_CAPACITY: usize = 10_000;

/// Why [`Mailbox::try_offer_now`] refused an item (the item is dropped).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfferError {
  /// The mailbox holds [`capacity`](Mailbox::capacity) user messages already.
  Full,
  /// The mailbox is closed (its process is dead or dying).
  Closed,
}

impl fmt::Display for OfferError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      OfferError::Full => f.write_str("mailbox full"),
      OfferError::Closed => f.write_str("mailbox closed"),
    }
  }
}

impl std::error::Error for OfferError {}

struct Entry<A> {
  item: A,
  /// Whether the item counts against the bound (`false` for system messages).
  bounded: bool,
}

struct State<A> {
  buf: VecDeque<Entry<A>>,
  /// Buffered items that count against `capacity`.
  bounded_len: usize,
  capacity: usize,
  closed: bool,
  /// The suspended consumer, if any.
  waker: Option<Waker>,
}

/// Bounded, single-consumer mailbox. Cloning yields another handle to the same mailbox.
pub struct Mailbox<A> {
  state: Arc<Mutex<State<A>>>,
}

impl<A> Clone for Mailbox<A> {
  fn clone(&self) -> Self {
    Self {
      state: Arc::clone(&self.state),
    }
  }
}

impl<A> fmt::Debug for Mailbox<A> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let st = self.lock();
    f.debug_struct("Mailbox")
      .field("len", &st.buf.len())
      .field("capacity", &st.capacity)
      .field("closed", &st.closed)
      .finish()
  }
}

impl<A> Default for Mailbox<A> {
  fn default() -> Self {
    Self::new()
  }
}

impl<A> Mailbox<A> {
  /// An empty, open mailbox bounded at [`DEFAULT_MAILBOX_CAPACITY`] user messages.
  pub fn new() -> Self {
    Self::with_capacity(DEFAULT_MAILBOX_CAPACITY)
  }

  /// An empty, open mailbox bounded at `capacity` user messages (at least one).
  pub fn with_capacity(capacity: usize) -> Self {
    Self {
      state: Arc::new(Mutex::new(State {
        buf: VecDeque::new(),
        bounded_len: 0,
        capacity: capacity.max(1),
        closed: false,
        waker: None,
      })),
    }
  }

  fn lock(&self) -> MutexGuard<'_, State<A>> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Enqueue a user message without waiting; `false` (and `item` dropped) when the mailbox is
  /// full or closed. [`try_offer_now`](Self::try_offer_now) says which.
  pub fn offer_now(&self, item: A) -> bool {
    self.try_offer_now(item).is_ok()
  }

  /// Enqueue a user message without waiting. Fails (and drops `item`) with
  /// [`OfferError::Full`] when [`capacity`](Self::capacity) user messages are buffered, and with
  /// [`OfferError::Closed`] once the mailbox is closed.
  pub fn try_offer_now(&self, item: A) -> Result<(), OfferError> {
    self.push(item, true)
  }

  /// Enqueue a system message (exit signal, monitor down) without waiting, bypassing the bound;
  /// `false` (and `item` dropped) once the mailbox is closed.
  pub fn offer_system_now(&self, item: A) -> bool {
    self.push(item, false).is_ok()
  }

  fn push(&self, item: A, bounded: bool) -> Result<(), OfferError> {
    let waker = {
      let mut st = self.lock();
      if st.closed {
        return Err(OfferError::Closed);
      }
      if bounded {
        if st.bounded_len >= st.capacity {
          return Err(OfferError::Full);
        }
        st.bounded_len += 1;
      }
      st.buf.push_back(Entry { item, bounded });
      st.waker.take()
    };
    if let Some(w) = waker {
      w.wake();
    }
    Ok(())
  }

  /// Close the mailbox to producers. Buffered items stay available to the consumer; once they
  /// are drained, [`take`](Self::take) fails with [`MailboxClosed`]. Idempotent.
  pub fn close(&self) {
    let waker = {
      let mut st = self.lock();
      st.closed = true;
      st.waker.take()
    };
    if let Some(w) = waker {
      w.wake();
    }
  }

  /// Whether [`close`](Self::close) has run.
  pub fn is_closed(&self) -> bool {
    self.lock().closed
  }

  /// The most user messages this mailbox buffers at once.
  pub fn capacity(&self) -> usize {
    self.lock().capacity
  }

  /// Number of buffered items (system messages included).
  pub fn len(&self) -> usize {
    self.lock().buf.len()
  }

  /// Whether no item is buffered.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }
}

impl<A: Send + 'static> Mailbox<A> {
  /// [`offer_now`](Self::offer_now) as an effect.
  pub fn offer<E, R>(&self, item: A) -> Effect<bool, E, R> {
    let mailbox = self.clone();
    sync(move || mailbox.offer_now(item))
  }

  /// Take the next item, suspending while the mailbox is empty and open.
  pub fn take<R: Send + Sync + 'static>(&self) -> Effect<A, MailboxClosed, R> {
    let mailbox = self.clone();
    from_async(move || Take { mailbox })
  }

  /// Take the first buffered item `matcher` accepts, leaving every skipped item in place (and
  /// in arrival order); suspends until a matching item arrives.
  ///
  /// The scan covers at most [`capacity`](Self::capacity) items: once that many have been
  /// skipped it fails with [`ReceiveError::ScanLimit`]. It fails with [`ReceiveError::Closed`]
  /// as soon as the mailbox is closed (the process is being killed). `matcher` runs under the
  /// mailbox lock: it must only inspect the item, never send to this mailbox.
  pub(crate) fn take_matching<R: Send + Sync + 'static>(
    &self,
    matcher: Matcher<A>,
  ) -> Effect<A, ReceiveError, R> {
    let mailbox = self.clone();
    from_async(move || TakeMatching {
      mailbox,
      matcher,
      scanned: 0,
    })
  }
}

impl<A> State<A> {
  fn remove(&mut self, index: usize) -> Option<A> {
    let entry = self.buf.remove(index)?;
    if entry.bounded {
      self.bounded_len -= 1;
    }
    Some(entry.item)
  }
}

struct Take<A> {
  mailbox: Mailbox<A>,
}

impl<A> Future for Take<A> {
  type Output = Result<A, MailboxClosed>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let mut st = self.mailbox.lock();
    if let Some(item) = st.remove(0) {
      return Poll::Ready(Ok(item));
    }
    if st.closed {
      return Poll::Ready(Err(MailboxClosed));
    }
    st.waker = Some(cx.waker().clone());
    Poll::Pending
  }
}

/// Predicate of a selective receive.
pub(crate) type Matcher<A> = Box<dyn FnMut(&A) -> bool + Send>;

struct TakeMatching<A> {
  mailbox: Mailbox<A>,
  matcher: Matcher<A>,
  /// Items at the front already rejected. Only this consumer removes items and producers only
  /// append, so they stay rejected and in place across polls.
  scanned: usize,
}

impl<A> Future for TakeMatching<A> {
  type Output = Result<A, ReceiveError>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = self.get_mut();
    let mut st = this.mailbox.lock();
    if st.closed {
      return Poll::Ready(Err(ReceiveError::Closed));
    }
    while this.scanned < st.buf.len() && this.scanned < st.capacity {
      if (this.matcher)(&st.buf[this.scanned].item) {
        let index = this.scanned;
        return match st.remove(index) {
          Some(item) => Poll::Ready(Ok(item)),
          None => Poll::Ready(Err(ReceiveError::Closed)),
        };
      }
      this.scanned += 1;
    }
    if this.scanned >= st.capacity {
      return Poll::Ready(Err(ReceiveError::ScanLimit));
    }
    st.waker = Some(cx.waker().clone());
    Poll::Pending
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Exit;
  use id_effect::{HNil, Runtime, TestExecutor};

  #[test]
  fn offer_take_close_drain() {
    let exec = TestExecutor::new();
    let rt = Runtime::from_executor(Arc::new(exec.clone()));
    let mb: Mailbox<u8> = Mailbox::new();
    assert!(mb.offer_now(1));
    let taker = rt.fork(mb.take::<HNil>().zip(mb.take()));
    exec.run_until_stalled();
    assert!(!taker.is_done(), "second take waits on an empty mailbox");
    assert!(mb.offer_now(2));
    exec.run_until_stalled();
    assert!(taker.is_done());
    mb.close();
    assert!(!mb.offer_now(3), "a closed mailbox rejects offers");
    assert!(mb.is_closed() && mb.is_empty());
    let closed = rt.fork(mb.take::<HNil>());
    exec.run_until_stalled();
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = std::pin::pin!(closed.join_future());
    assert!(matches!(
      fut.as_mut().poll(&mut cx),
      Poll::Ready(Exit::Failure(id_effect::Cause::Fail(MailboxClosed)))
    ));
  }

  #[test]
  fn close_wakes_a_waiting_take_and_offer_runs_as_an_effect() {
    let exec = TestExecutor::new();
    let rt = Runtime::from_executor(Arc::new(exec.clone()));
    let mb: Mailbox<u8> = Mailbox::default();
    let offered = rt.fork(mb.offer::<(), HNil>(7));
    exec.run_until_stalled();
    assert!(offered.is_done());
    assert_eq!(
      format!("{mb:?}"),
      "Mailbox { len: 1, capacity: 10000, closed: false }"
    );
    // The first take gets the buffered item; the second suspends until the close wakes it.
    let taker = rt.fork(mb.take::<HNil>().zip(mb.take()));
    exec.run_until_stalled();
    assert!(
      !taker.is_done(),
      "the second take waits on an empty mailbox"
    );
    mb.close();
    exec.run_until_stalled();
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = std::pin::pin!(taker.join_future());
    assert!(matches!(
      fut.as_mut().poll(&mut cx),
      Poll::Ready(Exit::Failure(id_effect::Cause::Fail(MailboxClosed)))
    ));
    assert_eq!(MailboxClosed.to_string(), "mailbox closed");
  }
}
