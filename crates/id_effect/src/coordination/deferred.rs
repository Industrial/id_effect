//! [`Deferred`]: a one-shot, multi-reader completion cell (Effect.ts `Deferred<A, E>`). Ported
//! from the v1 `coordination::deferred`, which was a tokio watch channel.
//!
//! The first completion wins (later attempts return `false`); [`Deferred::wait`] suspends every
//! waiter on a waker until then and re-raises the stored outcome, cause included.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use super::waiters::{WaitList, from_cause_future, lock, wake_all, with_fiber_id};
use crate::failure::{Cause, Defect, Exit};
use crate::next::kernel::{Effect, succeed, sync};

struct State<A, E> {
  exit: Option<Exit<A, E>>,
  waiters: WaitList<(), ()>,
}

struct Inner<A, E> {
  state: Mutex<State<A, E>>,
}

impl<A, E> Inner<A, E> {
  /// Stores `exit` when the cell is still empty (or always, with `force`); wakes every waiter.
  fn complete(&self, exit: Exit<A, E>, force: bool) -> bool {
    let ws = {
      let mut st = lock(&self.state);
      if st.exit.is_some() && !force {
        return false;
      }
      st.exit = Some(exit);
      st.waiters.release_all()
    };
    wake_all(ws);
    true
  }
}

/// A one-shot completion cell shared by any number of waiters. Cloning yields another handle to
/// the same cell.
///
/// ```
/// use id_effect::next::Deferred;
/// use id_effect::Never;
///
/// let d = Deferred::<u8, Never>::new();
/// let waiter = d.wait().fork();
/// let eff = waiter.flat_map(move |h| d.succeed(7).then(h.join()));
/// assert_eq!(eff.block_on().into_result().ok(), Some(7));
/// ```
pub struct Deferred<A, E> {
  inner: Arc<Inner<A, E>>,
}

impl<A, E> Clone for Deferred<A, E> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<A, E> std::fmt::Debug for Deferred<A, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let st = lock(&self.inner.state);
    f.debug_struct("Deferred")
      .field("done", &st.exit.is_some())
      .field("waiters", &st.waiters.len())
      .finish()
  }
}

impl<A, E> Default for Deferred<A, E>
where
  A: Clone + Send + 'static,
  E: Clone + Send + 'static,
{
  fn default() -> Self {
    Self::new()
  }
}

impl<A, E> Deferred<A, E>
where
  A: Clone + Send + 'static,
  E: Clone + Send + 'static,
{
  /// A new, incomplete deferred.
  pub fn new() -> Self {
    Self {
      inner: Arc::new(Inner {
        state: Mutex::new(State {
          exit: None,
          waiters: WaitList::default(),
        }),
      }),
    }
  }

  /// Suspends until the deferred is completed, then succeeds with its value or re-raises its
  /// cause (`Fail`, `Die` or `Interrupt`). Interrupting the waiter removes it.
  pub fn wait<R: Send + Sync + 'static>(&self) -> Effect<A, E, R> {
    let inner = Arc::clone(&self.inner);
    from_cause_future(move || async move { Wait { inner, id: None }.await.into_result() })
  }

  /// [`wait`](Self::wait) as a plain `Send + 'static` future of the stored [`Exit`].
  pub fn wait_future(&self) -> impl Future<Output = Exit<A, E>> + Send + 'static {
    Wait {
      inner: Arc::clone(&self.inner),
      id: None,
    }
  }

  /// Completes with `exit` now (outside an effect). `true` if this was the first completion.
  pub fn try_done(&self, exit: Exit<A, E>) -> bool {
    self.inner.complete(exit, false)
  }

  /// Completes with success now (outside an effect). `true` if this was the first completion.
  pub fn try_succeed(&self, value: A) -> bool {
    self.try_done(Exit::Success(value))
  }

  /// Completes with `cause` now (outside an effect). `true` if this was the first completion.
  pub fn try_fail_cause(&self, cause: Cause<E>) -> bool {
    self.try_done(Exit::Failure(cause))
  }

  /// Completes with `exit`. Succeeds with `true` if this was the first completion.
  pub fn done<E2, R>(&self, exit: Exit<A, E>) -> Effect<bool, E2, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || inner.complete(exit, false))
  }

  /// Completes with success.
  pub fn succeed<E2, R>(&self, value: A) -> Effect<bool, E2, R> {
    self.done(Exit::Success(value))
  }

  /// Completes with the typed failure `error`.
  pub fn fail<E2, R>(&self, error: E) -> Effect<bool, E2, R> {
    self.done(Exit::Failure(Cause::Fail(error)))
  }

  /// Completes with an arbitrary `cause`.
  pub fn fail_cause<E2, R>(&self, cause: Cause<E>) -> Effect<bool, E2, R> {
    self.done(Exit::Failure(cause))
  }

  /// Completes with a defect.
  pub fn die<E2, R>(&self, defect: impl Into<Defect>) -> Effect<bool, E2, R> {
    self.done(Exit::Failure(Cause::Die(defect.into())))
  }

  /// Completes with an interrupt by the calling fiber (Effect.ts `Deferred.interrupt`).
  pub fn interrupt<E2, R>(&self) -> Effect<bool, E2, R>
  where
    E2: Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    with_fiber_id(move |id| succeed(inner.complete(Exit::Failure(Cause::Interrupt(id)), false)))
  }

  /// Runs `eff` and completes with its outcome, success or failure (Effect.ts
  /// `Deferred.complete`). Succeeds with `true` if this was the first completion; `eff`'s
  /// failure is stored, not raised.
  pub fn complete<E2, R>(&self, eff: Effect<A, E, R>) -> Effect<bool, E2, R>
  where
    E2: Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    eff
      .exit::<E2>()
      .map(move |exit| inner.complete(exit, false))
  }

  /// Reads the stored outcome without waiting.
  pub fn poll<E2, R>(&self) -> Effect<Option<Exit<A, E>>, E2, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || lock(&inner.state).exit.clone())
  }

  /// `true` once completed.
  pub fn is_done<E2, R>(&self) -> Effect<bool, E2, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || lock(&inner.state).exit.is_some())
  }

  /// Stores `exit`, overwriting any earlier completion (use sparingly).
  pub fn unsafe_done<E2, R>(&self, exit: Exit<A, E>) -> Effect<(), E2, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || {
      inner.complete(exit, true);
    })
  }
}

/// The suspended wait.
struct Wait<A, E> {
  inner: Arc<Inner<A, E>>,
  id: Option<u64>,
}

impl<A: Clone, E: Clone> Future for Wait<A, E> {
  type Output = Exit<A, E>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Exit<A, E>> {
    let this = &mut *self;
    let mut st = lock(&this.inner.state);
    if let Some(exit) = &st.exit {
      let exit = exit.clone();
      if let Some(id) = this.id.take() {
        let _ = st.waiters.take_grant(id);
      }
      return Poll::Ready(exit);
    }
    match this.id {
      Some(id) => {
        st.waiters.refresh(id, cx.waker());
      }
      None => this.id = Some(st.waiters.push((), cx.waker())),
    }
    Poll::Pending
  }
}

impl<A, E> Drop for Wait<A, E> {
  fn drop(&mut self) {
    if let Some(id) = self.id.take() {
      let mut st = lock(&self.inner.state);
      if st.waiters.take_grant(id).is_none() {
        st.waiters.remove(id);
      }
    }
  }
}
