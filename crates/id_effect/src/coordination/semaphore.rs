//! N-permit gate — mirrors Effect.ts concurrency primitives backed by `tokio::sync::Semaphore`.
//!
//! Use [`Semaphore::acquire`] with [`crate::resource::scope::Scope`] as the effect environment `R` so the
//! permit is released when the scope is closed (via a [`Scope`] finalizer). [`Semaphore::try_acquire`]
//! returns an owned [`Permit`] that releases on drop instead.

use std::fmt;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore as TokioSemaphore};

use crate::kernel::{Effect, box_future};
use crate::resource::scope::Scope;
use crate::runtime::Never;

// ── Semaphore ───────────────────────────────────────────────────────────────

/// N-permit async semaphore (shared handle).
#[derive(Clone)]
pub struct Semaphore {
  inner: Arc<TokioSemaphore>,
}

impl Semaphore {
  /// `Semaphore.make(n)` → `Effect<Semaphore>` (lazy; no I/O until run).
  pub fn make(permits: usize) -> Effect<Self> {
    Effect::new(move |_| {
      Ok(Semaphore {
        inner: Arc::new(TokioSemaphore::new(permits)),
      })
    })
  }

  /// Acquire one permit, suspending if none are available.
  ///
  /// The acquired [`OwnedSemaphorePermit`] is stored in a [`Scope`] finalizer on `scope` (the
  /// effect environment `R`). Call with [`crate::runtime::run_async`]`(self.acquire(), scope.clone())`.
  /// When `scope` closes, the permit is released.
  ///
  /// The returned [`Permit`] is a zero-cost marker (`Permit(None)`); it does **not** own the permit.
  pub fn acquire(&self) -> Effect<Permit, Never, Scope> {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |scope: &mut Scope| {
      let scope = scope.clone();
      box_future(async move {
        let guard = inner.acquire_owned().await;
        let _added = scope.add_finalizer(Box::new(move |_exit| {
          Effect::new(move |_r| {
            drop(guard);
            Ok(())
          })
        }));
        // If the scope was already closed, `add_finalizer` drops the box and the permit is released.
        Ok(Permit(None))
      })
    })
  }

  /// Acquire one permit asynchronously; the returned [`Permit`] owns the handle and releases on
  /// [`Drop`] (no [`Scope`] required).
  pub fn acquire_owned(&self) -> Effect<Permit, Never, ()> {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |_env: &mut ()| {
      box_future(async move {
        let guard = inner
          .acquire_owned()
          .await
          .unwrap_or_else(|_| unreachable!("semaphore closed"));
        Ok(Permit(Some(guard)))
      })
    })
  }

  /// Try to acquire without blocking. On success, the returned [`Permit`] owns the permit and
  /// releases on drop.
  pub fn try_acquire(&self) -> Effect<Option<Permit>, Never, ()> {
    let inner = Arc::clone(&self.inner);
    Effect::new(move |_| Ok(inner.try_acquire_owned().ok().map(|g| Permit(Some(g)))))
  }

  /// Current number of available permits (best-effort; concurrent acquires may change it immediately after).
  pub fn available(&self) -> Effect<usize, Never, ()> {
    let n = self.inner.available_permits();
    Effect::new(move |_| Ok(n))
  }
}

// ── Permit ─────────────────────────────────────────────────────────────────

/// Proof of holding one semaphore permit, or an owned handle that releases on [`Drop`].
///
/// After [`Semaphore::acquire`], this is always `Permit(None)` — release happens via [`Scope`].
/// After [`Semaphore::try_acquire`], `Some(_)` holds an [`OwnedSemaphorePermit`].
pub struct Permit(Option<OwnedSemaphorePermit>);

impl fmt::Debug for Permit {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Permit")
      .field("owned", &self.0.is_some())
      .finish()
  }
}

// ── Free functions (Effect.ts-style) ────────────────────────────────────────

/// `Semaphore.make(n)`
pub fn make(permits: usize) -> Effect<Semaphore> {
  Semaphore::make(permits)
}

/// `Semaphore.acquire(sem)`
pub fn acquire(sem: &Semaphore) -> Effect<Permit, Never, Scope> {
  sem.acquire()
}

/// `Semaphore.acquire_owned(sem)`
pub fn acquire_owned(sem: &Semaphore) -> Effect<Permit, Never, ()> {
  sem.acquire_owned()
}

/// `Semaphore.try_acquire(sem)`
pub fn try_acquire(sem: &Semaphore) -> Effect<Option<Permit>, Never, ()> {
  sem.try_acquire()
}

/// `Semaphore.available(sem)`
pub fn available(sem: &Semaphore) -> Effect<usize, Never, ()> {
  sem.available()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::run_async;
  use std::sync::mpsc;
  use std::thread;
  use std::time::{Duration, Instant};

  #[test]
  fn semaphore_try_acquire_none_when_exhausted() {
    let sem = crate::runtime::run_blocking(Semaphore::make(0), ()).unwrap();
    assert!(
      crate::runtime::run_blocking(sem.try_acquire(), ())
        .unwrap()
        .is_none()
    );
  }

  #[test]
  fn semaphore_try_acquire_some_when_available() {
    let sem = crate::runtime::run_blocking(Semaphore::make(1), ()).unwrap();
    let p = crate::runtime::run_blocking(sem.try_acquire(), ())
      .unwrap()
      .expect("permit");
    drop(p);
    assert!(
      crate::runtime::run_blocking(sem.try_acquire(), ())
        .unwrap()
        .is_some()
    );
  }

  #[test]
  fn semaphore_permit_released_on_scope_close() {
    let sem = crate::runtime::run_blocking(Semaphore::make(1), ()).unwrap();
    let scope = Scope::make();
    let _ = pollster::block_on(run_async(sem.clone().acquire(), scope.clone())).unwrap();
    assert!(
      crate::runtime::run_blocking(sem.try_acquire(), ())
        .unwrap()
        .is_none()
    );
    scope.close();
    assert!(
      crate::runtime::run_blocking(sem.try_acquire(), ())
        .unwrap()
        .is_some()
    );
  }

  #[test]
  fn semaphore_acquire_blocks_when_zero_permits() {
    let sem = crate::runtime::run_blocking(Semaphore::make(1), ()).unwrap();
    let (tx, rx) = mpsc::channel::<()>();
    let sem_t = sem.clone();
    let th = thread::spawn(move || {
      let scope = Scope::make();
      pollster::block_on(run_async(sem_t.acquire(), scope.clone())).unwrap();
      tx.send(()).expect("signal acquire");
      thread::sleep(Duration::from_millis(150));
      scope.close();
    });
    rx.recv().expect("peer acquired");
    let scope_m = Scope::make();
    let start = Instant::now();
    pollster::block_on(run_async(sem.acquire(), scope_m.clone())).unwrap();
    assert!(
      start.elapsed() >= Duration::from_millis(80),
      "expected second acquire to block until first scope closed"
    );
    scope_m.close();
    th.join().expect("thread");
  }

  // ── acquire_owned ─────────────────────────────────────────────────────

  #[test]
  fn semaphore_acquire_owned_returns_permit() {
    let sem = crate::runtime::run_blocking(Semaphore::make(1), ()).unwrap();
    let permit = pollster::block_on(run_async(sem.acquire_owned(), ())).unwrap();
    assert_eq!(
      crate::runtime::run_blocking(sem.available(), ()).unwrap(),
      0,
      "permit should consume the slot"
    );
    drop(permit);
    assert_eq!(
      crate::runtime::run_blocking(sem.available(), ()).unwrap(),
      1,
      "permit release should restore the slot"
    );
  }

  // ── available ────────────────────────────────────────────────────────

  #[test]
  fn semaphore_available_tracks_permits() {
    let sem = crate::runtime::run_blocking(Semaphore::make(3), ()).unwrap();
    assert_eq!(
      crate::runtime::run_blocking(sem.available(), ()).unwrap(),
      3
    );
    let p = pollster::block_on(run_async(sem.acquire_owned(), ())).unwrap();
    assert_eq!(
      crate::runtime::run_blocking(sem.available(), ()).unwrap(),
      2
    );
    drop(p);
  }

  // ── free-function wrappers ─────────────────────────────────────────────

  #[test]
  fn semaphore_free_fn_make_and_acquire_owned() {
    let sem = crate::runtime::run_blocking(make(2), ()).unwrap();
    assert_eq!(
      crate::runtime::run_blocking(available(&sem), ()).unwrap(),
      2
    );
    let p1 = pollster::block_on(run_async(acquire_owned(&sem), ())).unwrap();
    assert_eq!(
      crate::runtime::run_blocking(available(&sem), ()).unwrap(),
      1
    );
    let p2 = crate::runtime::run_blocking(try_acquire(&sem), ()).unwrap();
    assert!(p2.is_some());
    assert_eq!(
      crate::runtime::run_blocking(available(&sem), ()).unwrap(),
      0
    );
    drop(p1);
    drop(p2);
  }

  #[test]
  fn semaphore_free_fn_acquire_with_scope() {
    let sem = crate::runtime::run_blocking(make(1), ()).unwrap();
    let scope = Scope::make();
    pollster::block_on(run_async(acquire(&sem), scope.clone())).unwrap();
    assert_eq!(
      crate::runtime::run_blocking(available(&sem), ()).unwrap(),
      0
    );
    scope.close();
    assert_eq!(
      crate::runtime::run_blocking(available(&sem), ()).unwrap(),
      1
    );
  }
}
