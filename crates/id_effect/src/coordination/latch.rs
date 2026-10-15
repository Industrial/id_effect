//! [`Latch`]: a gate fibers wait on (Effect.ts `Latch`). Ported from the v1
//! `coordination::latch`, which was a tokio `Notify` plus a flag.
//!
//! [`Latch::wait`] suspends while the latch is closed. [`Latch::open`] opens it and wakes every
//! waiter; [`Latch::release`] wakes the current waiters once without opening; [`Latch::close`]
//! closes it again.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use super::waiters::{WaitList, lock, wake_all};
use crate::kernel::{Effect, from_async, sync};

struct State {
  open: bool,
  waiters: WaitList<(), ()>,
}

/// A gate. Cloning yields another handle to the same latch.
#[derive(Clone)]
pub struct Latch {
  inner: Arc<Mutex<State>>,
}

impl std::fmt::Debug for Latch {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let st = lock(&self.inner);
    f.debug_struct("Latch")
      .field("open", &st.open)
      .field("waiters", &st.waiters.len())
      .finish()
  }
}

impl Default for Latch {
  fn default() -> Self {
    Self::new()
  }
}

impl Latch {
  /// A closed latch.
  pub fn new() -> Self {
    Self::with_state(false)
  }

  /// A latch that starts open (`true`) or closed (`false`).
  pub fn with_state(open: bool) -> Self {
    Self {
      inner: Arc::new(Mutex::new(State {
        open,
        waiters: WaitList::default(),
      })),
    }
  }

  fn set(&self, open: Option<bool>) {
    let ws = {
      let mut st = lock(&self.inner);
      if let Some(open) = open {
        st.open = open;
      }
      st.waiters.release_all()
    };
    wake_all(ws);
  }

  /// Opens the latch and wakes every waiter. Idempotent.
  pub fn open<E, R>(&self) -> Effect<(), E, R> {
    let l = self.clone();
    sync(move || l.set(Some(true)))
  }

  /// Wakes the fibers waiting right now without opening the latch (later waiters still wait).
  pub fn release<E, R>(&self) -> Effect<(), E, R> {
    let l = self.clone();
    sync(move || l.set(None))
  }

  /// Closes the latch: later [`wait`](Self::wait)s suspend again.
  pub fn close<E, R>(&self) -> Effect<(), E, R> {
    let l = self.clone();
    sync(move || lock(&l.inner).open = false)
  }

  /// Suspends until the latch is open (or [`release`](Self::release)d). Interrupting the waiter
  /// removes it.
  pub fn wait<E, R>(&self) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    from_async(move || async move {
      LatchWait { inner, id: None }.await;
      Ok(())
    })
  }

  /// Runs `eff` once the latch is open.
  pub fn when_open<A, E, R>(&self, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.wait().then(eff)
  }

  /// `true` while the latch is open.
  pub fn is_open<E, R>(&self) -> Effect<bool, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || lock(&inner).open)
  }
}

/// The suspended wait.
struct LatchWait {
  inner: Arc<Mutex<State>>,
  id: Option<u64>,
}

impl Future for LatchWait {
  type Output = ();

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    let this = &mut *self;
    let mut st = lock(&this.inner);
    match this.id {
      Some(id) => {
        if st.waiters.take_grant(id).is_some() {
          this.id = None;
          return Poll::Ready(());
        }
        st.waiters.refresh(id, cx.waker());
        Poll::Pending
      }
      None if st.open => Poll::Ready(()),
      None => {
        this.id = Some(st.waiters.push((), cx.waker()));
        Poll::Pending
      }
    }
  }
}

impl Drop for LatchWait {
  fn drop(&mut self) {
    if let Some(id) = self.id.take() {
      let mut st = lock(&self.inner);
      if st.waiters.take_grant(id).is_none() {
        st.waiters.remove(id);
      }
    }
  }
}
