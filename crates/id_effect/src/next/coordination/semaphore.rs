//! [`Semaphore`] and its RAII [`Permit`] (Effect.ts `Semaphore`). Ported from the v1
//! `coordination::semaphore`, without tokio's semaphore.
//!
//! Waiters queue FIFO: a waiter at the front that needs more permits than are free blocks the
//! ones behind it (no starvation of large acquires). A release hands permits directly to the
//! front waiters. An interrupted acquire is removed from the queue; one that was granted permits
//! but dropped before observing them gives them back.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use super::waiters::{WaitList, lock, wake_all};
use crate::next::kernel::{Effect, from_async, sync, uninterruptible_mask};
use crate::next::scope::Scope;
use crate::runtime::Never;

struct State {
  permits: usize,
  /// Payload: the number of permits the waiter needs.
  waiters: WaitList<usize, ()>,
}

impl State {
  /// Grants permits to the front waiters while they fit; returns the wakers to wake.
  fn grant_ready(&mut self) -> Vec<Waker> {
    let mut out = Vec::new();
    while let Some(&need) = self.waiters.front() {
      if need > self.permits {
        break;
      }
      self.permits -= need;
      if let Ok((_, w)) = self.waiters.grant_front(()) {
        out.push(w);
      }
    }
    out
  }
}

struct Inner {
  state: Mutex<State>,
}

impl Inner {
  fn release(&self, n: usize) {
    let ws = {
      let mut st = lock(&self.state);
      st.permits += n;
      st.grant_ready()
    };
    wake_all(ws);
  }
}

/// An N-permit semaphore. Cloning yields another handle to the same semaphore.
///
/// [`acquire`](Self::acquire) suspends the fiber (no thread is blocked) until a permit is free
/// and returns a [`Permit`] that releases on drop. [`with_permits`](Self::with_permits) is the
/// structured form: the permits are released however the effect ends.
#[derive(Clone)]
pub struct Semaphore {
  inner: Arc<Inner>,
}

impl std::fmt::Debug for Semaphore {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let st = lock(&self.inner.state);
    f.debug_struct("Semaphore")
      .field("available", &st.permits)
      .field("waiters", &st.waiters.len())
      .finish()
  }
}

impl Semaphore {
  /// A semaphore with `permits` free permits.
  pub fn new(permits: usize) -> Self {
    Self {
      inner: Arc::new(Inner {
        state: Mutex::new(State {
          permits,
          waiters: WaitList::default(),
        }),
      }),
    }
  }

  /// Acquires one permit, suspending until it is free.
  pub fn acquire<E, R>(&self) -> Effect<Permit, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.acquire_n(1)
  }

  /// Acquires `n` permits at once, suspending until they are free. `n == 0` succeeds at once.
  pub fn acquire_n<E, R>(&self, n: usize) -> Effect<Permit, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    from_async(move || async move {
      Ok(
        Acquire {
          inner,
          need: n,
          id: None,
        }
        .await,
      )
    })
  }

  /// Takes one permit if one is free right now (and no waiter is queued ahead).
  pub fn try_acquire<E, R>(&self) -> Effect<Option<Permit>, E, R> {
    self.try_acquire_n(1)
  }

  /// Takes `n` permits if they are free right now (and no waiter is queued ahead).
  pub fn try_acquire_n<E, R>(&self, n: usize) -> Effect<Option<Permit>, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || {
      let mut st = lock(&inner.state);
      if st.waiters.is_empty() && st.permits >= n {
        st.permits -= n;
        drop(st);
        Some(Permit::new(inner, n))
      } else {
        None
      }
    })
  }

  /// The number of free permits (a snapshot).
  pub fn available<E, R>(&self) -> Effect<usize, E, R> {
    let inner = Arc::clone(&self.inner);
    sync(move || lock(&inner.state).permits)
  }

  /// Runs `eff` holding one permit (Effect.ts `withPermits(1)`).
  pub fn with_permit<A, E, R>(&self, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.with_permits(1, eff)
  }

  /// Runs `eff` holding `n` permits, released however `eff` ends (success, failure, defect or
  /// interrupt). The wait for the permits is interruptible; the hand-over is not, so a permit is
  /// never lost between acquiring it and running `eff`.
  pub fn with_permits<A, E, R>(&self, n: usize, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let sem = self.clone();
    uninterruptible_mask(move |restore| {
      restore
        .apply(sem.acquire_n::<E, R>(n))
        .flat_map(move |permit| restore.apply(eff).ensuring(sync(move || drop(permit))))
    })
  }

  /// Acquires one permit and ties it to `scope`: it is released when the scope closes (the v1
  /// `acquire` under a `Scope` environment). On a closed scope the permit is released at once.
  pub fn acquire_scoped<E, R>(&self, scope: &Scope) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let sem = self.clone();
    let scope = scope.clone();
    uninterruptible_mask(move |restore| {
      restore
        .apply(sem.acquire::<E, R>())
        .flat_map(move |permit| scope.add_finalizer(sync::<(), Never, _>(move || drop(permit))))
    })
  }
}

/// Holds permits of a [`Semaphore`]; releases them when dropped (or on [`release`](Self::release)).
pub struct Permit {
  inner: Arc<Inner>,
  n: usize,
}

impl std::fmt::Debug for Permit {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Permit").field("permits", &self.n).finish()
  }
}

impl Permit {
  fn new(inner: Arc<Inner>, n: usize) -> Self {
    Self { inner, n }
  }

  /// How many permits this guard holds.
  pub fn permits(&self) -> usize {
    self.n
  }

  /// Releases the permits now (same as dropping the guard).
  pub fn release(self) {
    drop(self);
  }
}

impl Drop for Permit {
  fn drop(&mut self) {
    if self.n > 0 {
      self.inner.release(self.n);
    }
  }
}

/// The suspended acquire: queued once, granted by a release, cancelled on drop.
struct Acquire {
  inner: Arc<Inner>,
  need: usize,
  id: Option<u64>,
}

impl Future for Acquire {
  type Output = Permit;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Permit> {
    let this = &mut *self;
    let mut st = lock(&this.inner.state);
    match this.id {
      Some(id) => {
        if st.waiters.take_grant(id).is_some() {
          this.id = None;
          drop(st);
          return Poll::Ready(Permit::new(Arc::clone(&this.inner), this.need));
        }
        st.waiters.refresh(id, cx.waker());
        Poll::Pending
      }
      None => {
        if st.waiters.is_empty() && st.permits >= this.need {
          st.permits -= this.need;
          drop(st);
          return Poll::Ready(Permit::new(Arc::clone(&this.inner), this.need));
        }
        this.id = Some(st.waiters.push(this.need, cx.waker()));
        Poll::Pending
      }
    }
  }
}

impl Drop for Acquire {
  fn drop(&mut self) {
    let Some(id) = self.id.take() else { return };
    let ws = {
      let mut st = lock(&self.inner.state);
      if st.waiters.take_grant(id).is_some() {
        // Granted but never observed (interrupted after the hand-over): give the permits back.
        st.permits += self.need;
      } else {
        st.waiters.remove(id);
      }
      // Removing a large waiter at the front may unblock the ones behind it.
      st.grant_ready()
    };
    wake_all(ws);
  }
}
