//! [`Mailbox`] — a process's unbounded, single-consumer message queue.
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

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use id_effect::{Effect, from_async, sync};

/// A take on a mailbox that is closed and drained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailboxClosed;

impl fmt::Display for MailboxClosed {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("mailbox closed")
  }
}

impl std::error::Error for MailboxClosed {}

struct State<A> {
  buf: VecDeque<A>,
  closed: bool,
  /// The suspended consumer, if any.
  waker: Option<Waker>,
}

/// Unbounded, single-consumer mailbox. Cloning yields another handle to the same mailbox.
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
  /// An empty, open mailbox.
  pub fn new() -> Self {
    Self {
      state: Arc::new(Mutex::new(State {
        buf: VecDeque::new(),
        closed: false,
        waker: None,
      })),
    }
  }

  fn lock(&self) -> MutexGuard<'_, State<A>> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Enqueue `item` without waiting; `false` (and `item` dropped) once the mailbox is closed.
  pub fn offer_now(&self, item: A) -> bool {
    let waker = {
      let mut st = self.lock();
      if st.closed {
        return false;
      }
      st.buf.push_back(item);
      st.waker.take()
    };
    if let Some(w) = waker {
      w.wake();
    }
    true
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

  /// Number of buffered items.
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
}

struct Take<A> {
  mailbox: Mailbox<A>,
}

impl<A> Future for Take<A> {
  type Output = Result<A, MailboxClosed>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let mut st = self.mailbox.lock();
    if let Some(item) = st.buf.pop_front() {
      return Poll::Ready(Ok(item));
    }
    if st.closed {
      return Poll::Ready(Err(MailboxClosed));
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
    assert_eq!(format!("{mb:?}"), "Mailbox { len: 1, closed: false }");
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
