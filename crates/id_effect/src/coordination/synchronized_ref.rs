//! [`tokio::sync::Mutex`]-backed cell with **serialized** async mutations.
//!
//! Unlike [`crate::coordination::ref_::Ref`], [`SynchronizedRef::modify_effect`], [`SynchronizedRef::update_effect`],
//! and [`SynchronizedRef::get_effect`] keep the mutex locked until the inner [`Effect`] completes,
//! so concurrent callers cannot interleave.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::kernel::{Effect, box_future};

// ── SynchronizedRef ─────────────────────────────────────────────────────────

/// Shared mutable cell using an async mutex; effectful updates are strictly serialized.
#[derive(Clone)]
pub struct SynchronizedRef<A> {
  inner: Arc<Mutex<A>>,
}

impl<A: Send + 'static> SynchronizedRef<A> {
  /// Direct construction (same cell as [`Self::make`], without wrapping in [`Effect`]).
  #[inline]
  pub fn new(initial: A) -> Self {
    Self {
      inner: Arc::new(Mutex::new(initial)),
    }
  }

  /// `SynchronizedRef.make(initial)` → `Effect<SynchronizedRef<A>>`
  pub fn make(initial: A) -> Effect<SynchronizedRef<A>> {
    Effect::new(move |_r| {
      Ok(SynchronizedRef {
        inner: Arc::new(Mutex::new(initial)),
      })
    })
  }

  /// Read the current value (clone).
  pub fn get(&self) -> Effect<A>
  where
    A: Clone,
  {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let g = inner.lock().await;
        Ok(g.clone())
      })
    })
  }

  /// Replace the whole value.
  pub fn set(&self, value: A) -> Effect<()> {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        *inner.lock().await = value;
        Ok(())
      })
    })
  }

  /// Synchronous transform while holding the lock (no `.await` inside `f`).
  pub fn update(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<()>
  where
    A: Clone,
  {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let mut g = inner.lock().await;
        let old = g.clone();
        *g = f(old);
        Ok(())
      })
    })
  }

  /// Run an effect to compute the next value; mutex stays held until the inner effect finishes.
  pub fn update_effect<E, R>(
    &self,
    f: impl FnOnce(A) -> Effect<A, E, R> + Send + 'static,
  ) -> Effect<(), E, R>
  where
    A: Clone + Send + 'static,
    E: 'static,
    R: 'static,
  {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |r: &mut R| {
      box_future(async move {
        let mut guard = inner.lock().await;
        let current = guard.clone();
        let new_val = f(current).run(r).await?;
        *guard = new_val;
        Ok(())
      })
    })
  }

  /// `f` returns `(output, new_state)`; lock held for the whole synchronous `f`.
  pub fn modify<B>(&self, f: impl FnOnce(A) -> (B, A) + Send + 'static) -> Effect<B>
  where
    A: Clone + Send + 'static,
    B: Send + 'static,
  {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let mut g = inner.lock().await;
        let (b, new_a) = f(g.clone());
        *g = new_a;
        Ok(b)
      })
    })
  }

  /// Like [`SynchronizedRef::modify`], but `f` returns an [`Effect`]; lock held across `.await`.
  pub fn modify_effect<B, E, R>(
    &self,
    f: impl FnOnce(A) -> Effect<(B, A), E, R> + Send + 'static,
  ) -> Effect<B, E, R>
  where
    A: Clone + Send + 'static,
    B: Send + 'static,
    E: 'static,
    R: 'static,
  {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |r: &mut R| {
      box_future(async move {
        let mut guard = inner.lock().await;
        let current = guard.clone();
        let (b, new_a) = f(current).run(r).await?;
        *guard = new_a;
        Ok(b)
      })
    })
  }

  /// Run an effect from the current value; mutex stays held until completion; stored value unchanged.
  pub fn get_effect<E, R>(
    &self,
    f: impl FnOnce(A) -> Effect<A, E, R> + Send + 'static,
  ) -> Effect<A, E, R>
  where
    A: Clone + Send + 'static,
    E: 'static,
    R: 'static,
  {
    let inner = Arc::clone(&self.inner);
    Effect::new_async(move |r: &mut R| {
      box_future(async move {
        let _guard = inner.lock().await;
        let current = _guard.clone();
        f(current).run(r).await
      })
    })
  }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::{run_async, run_blocking};
  use core::convert::Infallible;
  use std::time::Duration;

  #[test]
  fn sync_ref_update_effect_is_sequential_under_concurrency() {
    let s = run_blocking(SynchronizedRef::make(0_i32), ()).expect("make");
    let delay = Duration::from_millis(15);
    let s2 = s.clone();
    let h1 = std::thread::spawn(move || {
      run_blocking(
        s2.update_effect(move |n| {
          Effect::<i32, Infallible, ()>::new(move |_r: &mut ()| {
            std::thread::sleep(delay);
            Ok(n + 1)
          })
        }),
        (),
      )
    });
    let s3 = s.clone();
    let h2 = std::thread::spawn(move || {
      run_blocking(
        s3.update_effect(move |n| {
          Effect::<i32, Infallible, ()>::new(move |_r: &mut ()| {
            std::thread::sleep(delay);
            Ok(n + 1)
          })
        }),
        (),
      )
    });
    h1.join().expect("t1").expect("e1");
    h2.join().expect("t2").expect("e2");
    let v = run_blocking(s.get(), ()).expect("get");
    assert_eq!(v, 2);
  }

  #[tokio::test]
  async fn sync_ref_get_returns_current() {
    let s = run_async(SynchronizedRef::make(10_u32), ())
      .await
      .expect("make");
    assert_eq!(run_async(s.get(), ()).await.expect("get"), 10);
    run_async(s.set(20), ()).await.expect("set");
    assert_eq!(run_async(s.get(), ()).await.expect("get 2"), 20);
  }

  #[tokio::test]
  async fn sync_ref_modify_returns_computed_value() {
    let s = run_async(SynchronizedRef::make(3_i64), ())
      .await
      .expect("make");
    let b = run_async(s.modify(|n| (n * 2, n + 1)), ())
      .await
      .expect("modify");
    assert_eq!(b, 6);
    assert_eq!(run_async(s.get(), ()).await.expect("get"), 4);
  }

  // ── update (synchronous transform) ────────────────────────────────────

  #[tokio::test]
  async fn sync_ref_update_applies_fn_in_place() {
    let s = run_async(SynchronizedRef::make(5_u32), ())
      .await
      .expect("make");
    run_async(s.update(|n| n * 3), ()).await.expect("update");
    assert_eq!(run_async(s.get(), ()).await.expect("get"), 15);
  }

  // ── modify_effect ─────────────────────────────────────────────────────

  #[tokio::test]
  async fn sync_ref_modify_effect_returns_computed_value() {
    use crate::kernel::succeed;
    let s = run_async(SynchronizedRef::make(10_i32), ())
      .await
      .expect("make");
    let result = run_async(
      s.modify_effect(|n| succeed::<(i32, i32), core::convert::Infallible, ()>((n * 2, n + 5))),
      (),
    )
    .await
    .expect("modify_effect");
    assert_eq!(result, 20); // returned value
    assert_eq!(run_async(s.get(), ()).await.expect("get"), 15); // new state
  }

  // ── get_effect ────────────────────────────────────────────────────────

  #[tokio::test]
  async fn sync_ref_get_effect_applies_fn_to_current_value() {
    use crate::kernel::succeed;
    let s = run_async(SynchronizedRef::make(7_u32), ())
      .await
      .expect("make");
    let result = run_async(
      s.get_effect(|n| succeed::<u32, core::convert::Infallible, ()>(n * 2)),
      (),
    )
    .await
    .expect("get_effect");
    assert_eq!(result, 14); // 7 * 2, returned value
    // State unchanged
    assert_eq!(run_async(s.get(), ()).await.expect("get"), 7);
  }

  // ── SynchronizedRef::new (direct constructor) ─────────────────────────

  #[test]
  fn sync_ref_new_creates_ref_directly() {
    let s = SynchronizedRef::new(42_u32);
    assert_eq!(run_blocking(s.get(), ()).expect("get"), 42);
  }
}
