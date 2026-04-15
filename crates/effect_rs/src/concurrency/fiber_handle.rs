//! [`FiberHandle`], [`FiberStatus`], and fiber utility functions (§ 7.2–7.5).
//!
//! Composed from Stratum 4 ([`crate::failure`]), Stratum 6 ([`crate::runtime`] execution
//! primitives), and the [`FiberId`](super::fiber_id::FiberId) primitive from this stratum.

use core::marker::PhantomData;

use crate::coordination::deferred::Deferred;
use crate::failure::cause::Cause;
use crate::failure::exit::Exit;
use crate::kernel::{Effect, box_future};
use crate::resource::scope::Scope;

use super::fiber_id::FiberId;
use crate::runtime::{Never, run_async, run_blocking};

/// Runtime-owned handle to a spawned fiber.
///
/// Completion is backed by [`Deferred`] (watch-channel rendezvous). [`Self::join`] and
/// [`Self::await_exit`] surface completion; [`Self::await_exit`] returns the full [`Exit`] value.
#[derive(Debug, Clone)]
pub struct FiberHandle<A, E> {
  id: FiberId,
  deferred: Deferred<A, E>,
  _pd: PhantomData<fn() -> (A, E)>,
}

/// Snapshot of whether a [`FiberHandle`] has finished and how.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FiberStatus {
  /// Still running or not yet observed as complete.
  Running,
  /// Completed with success.
  Succeeded,
  /// Completed with a failure [`Cause`].
  Failed,
  /// Completed due to interrupt.
  Interrupted,
}

impl<A, E> FiberHandle<A, E>
where
  A: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
{
  /// Handle for a fiber not yet completed (waits on internal [`Deferred`]).
  #[inline]
  pub fn pending(id: FiberId) -> Self {
    let deferred = run_blocking(Deferred::make(), ()).expect("fiber deferred make");
    Self {
      id,
      deferred,
      _pd: PhantomData,
    }
  }

  /// Handle already completed with `result`.
  #[inline]
  pub fn completed(id: FiberId, result: Result<A, E>) -> Self {
    let deferred = run_blocking(Deferred::make(), ()).expect("fiber deferred make");
    let exit = match result {
      Ok(a) => Exit::succeed(a),
      Err(e) => Exit::fail(e),
    };
    run_blocking(deferred.unsafe_done(exit), ()).expect("fiber deferred unsafe_done");
    Self {
      id,
      deferred,
      _pd: PhantomData,
    }
  }

  /// Fiber id assigned at creation.
  #[inline]
  pub fn id(&self) -> FiberId {
    self.id
  }

  /// Complete the underlying deferred with `result` (for tests / manual drivers).
  pub fn mark_completed(&self, result: Result<A, E>) {
    match result {
      Ok(a) => {
        let _ = run_blocking(self.deferred.succeed(a), ());
      }
      Err(e) => {
        let _ = run_blocking(self.deferred.fail(e), ());
      }
    }
  }

  /// Fail the fiber with [`Cause::Interrupt`]. Returns `false` if already completed.
  pub fn interrupt(&self) -> bool {
    run_blocking(self.deferred.fail_cause(Cause::Interrupt(self.id)), ()).expect("fiber interrupt")
  }

  /// Running vs terminal outcome (blocking poll of the deferred).
  pub fn status(&self) -> FiberStatus {
    let Some(exit) = run_blocking(self.deferred.poll(), ()).expect("fiber poll") else {
      return FiberStatus::Running;
    };
    match exit {
      Exit::Success(_) => FiberStatus::Succeeded,
      Exit::Failure(Cause::Interrupt(_)) => FiberStatus::Interrupted,
      Exit::Failure(Cause::Fail(_)) => FiberStatus::Failed,
      Exit::Failure(_) => FiberStatus::Failed,
    }
  }

  /// `true` when [`Self::status`] is not [`FiberStatus::Running`].
  #[inline]
  pub fn is_done(&self) -> bool {
    self.status() != FiberStatus::Running
  }

  /// Non-blocking read of the current [`Exit`], if the fiber has completed.
  #[inline]
  pub fn poll(&self) -> Effect<Option<Exit<A, E>>, Never, ()> {
    self.deferred.poll()
  }

  /// Snapshot for callers that want the legacy `Option<Result<_, Cause>>` shape: `None` while
  /// running **or** when the stored exit is an interrupt (matches older `FiberHandle::poll`).
  #[inline]
  pub fn poll_result(&self) -> Option<Result<A, Cause<E>>> {
    let exit = run_blocking(self.deferred.poll(), ()).expect("fiber poll")?;
    match exit {
      Exit::Failure(Cause::Interrupt(_)) => None,
      other => Some(other.into_result()),
    }
  }

  /// Wait for completion and return the full [`Exit`] (success or failure [`Cause`]).
  #[inline]
  pub fn await_exit(&self) -> Effect<Exit<A, E>, Never, ()> {
    let d = self.deferred.clone();
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        match d.wait().run(&mut ()).await {
          Ok(a) => Ok(Exit::Success(a)),
          Err(c) => Ok(Exit::Failure(c)),
        }
      })
    })
  }

  /// Await completion and map to `Result` (interrupt surfaces as [`Cause::Interrupt`]).
  #[inline]
  pub async fn join(&self) -> Result<A, Cause<E>> {
    run_async(self.await_exit(), ())
      .await
      .expect("await_exit is infallible")
      .into_result()
  }

  /// Map a successful value after this fiber completes (spawns a background join on the tokio
  /// runtime).
  pub fn map<B>(self, f: impl Fn(A) -> B + Send + Sync + 'static) -> FiberHandle<B, E>
  where
    B: Clone + Send + Sync + 'static,
  {
    let out = FiberHandle::<B, E>::pending(FiberId::fresh());
    let d_in = self.deferred.clone();
    let d_out = out.deferred.clone();
    tokio::spawn(async move {
      match d_in.wait_future().await {
        Ok(a) => {
          let _ = d_out.try_succeed(f(a));
        }
        Err(c) => {
          let _ = d_out.try_fail_cause(c);
        }
      }
    });
    out
  }

  /// Zip two fibers; both must succeed. The first failure [`Cause`] wins.
  pub fn zip<B>(self, other: FiberHandle<B, E>) -> FiberHandle<(A, B), E>
  where
    B: Clone + Send + Sync + 'static,
  {
    let out = FiberHandle::<(A, B), E>::pending(FiberId::fresh());
    let d_out = out.deferred.clone();
    let a = self.deferred.clone();
    let b = other.deferred.clone();
    tokio::spawn(async move {
      let (ra, rb) = tokio::join!(a.wait_future(), b.wait_future());
      match (ra, rb) {
        (Ok(x), Ok(y)) => {
          let _ = d_out.try_succeed((x, y));
        }
        (Err(c), _) | (_, Err(c)) => {
          let _ = d_out.try_fail_cause(c);
        }
      }
    });
    out
  }

  /// Like [`Self::zip`], then combine success values with `f`.
  pub fn zip_with<B, C, F>(self, other: FiberHandle<B, E>, f: F) -> FiberHandle<C, E>
  where
    B: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
    F: Fn(A, B) -> C + Send + Sync + 'static,
  {
    self.zip(other).map(move |(a, b)| f(a, b))
  }

  /// Race: complete with whichever fiber finishes first (the other keeps running for other
  /// waiters).
  pub fn or_else(self, other: FiberHandle<A, E>) -> FiberHandle<A, E> {
    let out = FiberHandle::<A, E>::pending(FiberId::fresh());
    let d_out = out.deferred.clone();
    let a = self.deferred.clone();
    let b = other.deferred.clone();
    tokio::spawn(async move {
      let winner = tokio::select! {
        r = a.wait_future() => r,
        r = b.wait_future() => r,
      };
      match winner {
        Ok(v) => {
          let _ = d_out.try_succeed(v);
        }
        Err(c) => {
          let _ = d_out.try_fail_cause(c);
        }
      }
    });
    out
  }

  /// Register a scope finalizer that interrupts this fiber on scope close, then await completion
  /// under the given [`Scope`] environment.
  pub fn scoped(self) -> Effect<A, Cause<E>, Scope> {
    let handle = self.clone();
    Effect::new_async(move |scope: &mut Scope| {
      let scope = scope.clone();
      let h = handle.clone();
      box_future(async move {
        let added = scope.add_finalizer(Box::new(move |_exit: Exit<(), Never>| {
          Effect::new(move |_r: &mut ()| {
            let _ = h.interrupt();
            Ok::<(), Never>(())
          })
        }));
        if !added {
          let _ = handle.interrupt();
        }
        handle.deferred.wait_future().await
      })
    })
  }

  /// Request interrupt from the async runtime without blocking the caller.
  pub fn interrupt_fork(&self) -> Effect<(), Never, ()> {
    let h = self.clone();
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        tokio::spawn(async move {
          let _ = h.interrupt();
        });
        Ok(())
      })
    })
  }
}

/// Join every handle in order; fails on the first failure [`Cause`].
pub fn fiber_all<A, E>(handles: Vec<FiberHandle<A, E>>) -> Effect<Vec<A>, Cause<E>, ()>
where
  A: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
{
  Effect::new_async(move |_r: &mut ()| {
    box_future(async move {
      let mut out = Vec::with_capacity(handles.len());
      for h in handles {
        out.push(h.deferred.wait_future().await?);
      }
      Ok(out)
    })
  })
}

/// Interrupt each handle (typically used for cleanup).
pub fn interrupt_all<A, E>(handles: Vec<FiberHandle<A, E>>) -> Effect<(), Never, ()>
where
  A: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
{
  Effect::new(move |_r: &mut ()| {
    for h in handles {
      let _ = h.interrupt();
    }
    Ok::<(), Never>(())
  })
}

/// Completed fiber with success value `a`.
#[inline]
pub fn fiber_succeed<A, E>(id: FiberId, a: A) -> FiberHandle<A, E>
where
  A: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
{
  FiberHandle::completed(id, Ok(a))
}

/// Fiber that never completes until interrupted or externally completed.
#[inline]
pub fn fiber_never<A, E>(id: FiberId) -> FiberHandle<A, E>
where
  A: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
{
  FiberHandle::pending(id)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  mod fiber_handle {
    use super::*;

    #[test]
    fn join_when_handle_is_completed_is_idempotent() {
      let handle = FiberHandle::completed(FiberId::fresh(), Ok::<u8, ()>(42));
      let first = pollster::block_on(handle.join());
      let second = pollster::block_on(handle.join());
      assert_eq!(first, Ok(42));
      assert_eq!(second, Ok(42));
    }

    #[test]
    fn interrupt_when_called_twice_is_idempotent_and_marks_handle_done() {
      let handle = FiberHandle::<u8, ()>::pending(FiberId::fresh());
      assert_eq!(handle.status(), FiberStatus::Running);
      assert!(!handle.is_done());

      assert!(handle.interrupt());
      assert!(!handle.interrupt());
      assert_eq!(handle.status(), FiberStatus::Interrupted);
      assert!(handle.is_done());
    }

    #[test]
    fn status_when_handle_completed_reflects_success_and_failure() {
      let ok = FiberHandle::completed(FiberId::fresh(), Ok::<u8, ()>(1));
      let err = FiberHandle::completed(FiberId::fresh(), Err::<u8, _>("boom"));

      assert_eq!(ok.status(), FiberStatus::Succeeded);
      assert_eq!(err.status(), FiberStatus::Failed);
    }

    #[test]
    fn poll_returns_none_when_running() {
      let handle = FiberHandle::<u8, ()>::pending(FiberId::fresh());
      assert!(handle.poll_result().is_none());
    }

    #[test]
    fn poll_returns_some_on_completion() {
      let handle = FiberHandle::completed(FiberId::fresh(), Ok::<u8, ()>(99));
      assert_eq!(handle.poll_result(), Some(Ok(99)));
    }

    #[test]
    fn fiber_join_waits_for_completion() {
      let handle = FiberHandle::<u8, ()>::pending(FiberId::fresh());
      let producer = handle.clone();
      std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1));
        producer.mark_completed(Ok(7));
      });
      let out = pollster::block_on(handle.join());
      assert_eq!(out, Ok(7));
    }

    #[test]
    fn fiber_interrupt_delivers_cause() {
      let id = FiberId::fresh();
      let handle = FiberHandle::<u8, ()>::pending(id);
      assert!(handle.interrupt());
      let out = pollster::block_on(handle.join());
      assert_eq!(out, Err(Cause::Interrupt(id)));
    }

    #[test]
    fn fiber_status_pending_then_done() {
      let handle = FiberHandle::<u8, ()>::pending(FiberId::fresh());
      assert_eq!(handle.status(), FiberStatus::Running);
      handle.mark_completed(Ok(11));
      assert_eq!(handle.status(), FiberStatus::Succeeded);
      assert!(handle.is_done());
    }

    #[test]
    fn poll_result_can_chain_with_option_helpers() {
      use crate::foundation::option_::option;

      let pending = FiberHandle::<u8, ()>::pending(FiberId::fresh());
      let mapped = option::map(pending.poll_result(), |r| r.map(|v| v + 1));
      assert_eq!(mapped, None);

      let done = FiberHandle::completed(FiberId::fresh(), Ok::<u8, ()>(3));
      let doubled = option::map(done.poll_result(), |r| r.map(|v| v * 2));
      assert_eq!(doubled, Some(Ok(6_u8)));

      let fallback = option::get_or_else(
        FiberHandle::<u8, ()>::pending(FiberId::fresh()).poll_result(),
        || Ok(0_u8),
      );
      assert_eq!(fallback, Ok(0_u8));
    }

    #[tokio::test]
    async fn fiber_await_exit_returns_full_exit() {
      let handle = FiberHandle::completed(FiberId::fresh(), Ok::<u8, ()>(5));
      let ex = run_async(handle.await_exit(), ())
        .await
        .expect("await_exit infallible");
      assert_eq!(ex.into_result(), Ok(5));
    }

    #[tokio::test]
    async fn fiber_zip_collects_both_results() {
      let a = FiberHandle::<u8, ()>::completed(FiberId::fresh(), Ok(1));
      let b = FiberHandle::<u8, ()>::completed(FiberId::fresh(), Ok(2));
      let z = a.zip(b);
      assert_eq!(z.join().await.unwrap(), (1, 2));
    }

    #[tokio::test]
    async fn fiber_or_else_returns_faster() {
      let slow = FiberHandle::<u8, ()>::pending(FiberId::fresh());
      let fast = FiberHandle::<u8, ()>::completed(FiberId::fresh(), Ok(42));
      let raced = slow.or_else(fast);
      assert_eq!(raced.join().await.unwrap(), 42);
    }

    #[test]
    fn fiber_scoped_interrupts_on_scope_close() {
      let id = FiberId::fresh();
      let scope = crate::resource::scope::Scope::make();
      let scope_for_close = scope.clone();
      let handle = FiberHandle::<u8, ()>::pending(id);
      let h = handle.clone();
      let worker = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(run_async(h.scoped(), scope))
      });
      std::thread::sleep(Duration::from_millis(50));
      assert!(scope_for_close.close());
      let out = worker.join().expect("scoped worker join");
      assert_eq!(out, Err(Cause::Interrupt(id)));
      assert_eq!(pollster::block_on(handle.join()), Err(Cause::Interrupt(id)));
    }

    // ── id ────────────────────────────────────────────────────────────────

    #[test]
    fn fiber_id_is_accessible() {
      let id = FiberId::fresh();
      let handle = FiberHandle::<u8, ()>::pending(id);
      assert_eq!(handle.id(), id);
    }

    // ── poll (Effect API) ─────────────────────────────────────────────────

    #[test]
    fn fiber_poll_effect_returns_none_when_running_and_some_when_done() {
      let handle = FiberHandle::<u8, ()>::pending(FiberId::fresh());
      let result = run_blocking(handle.poll(), ()).expect("poll infallible");
      assert!(
        result.is_none(),
        "pending fiber should return None from poll"
      );

      let done = FiberHandle::completed(FiberId::fresh(), Ok::<u8, ()>(77));
      let result = run_blocking(done.poll(), ()).expect("poll infallible");
      assert!(result.is_some());
      assert_eq!(result.unwrap().into_result(), Ok(77));
    }

    // ── map ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fiber_map_transforms_success_value() {
      let handle = FiberHandle::completed(FiberId::fresh(), Ok::<u8, ()>(5));
      let mapped = handle.map(|v| v * 3);
      assert_eq!(mapped.join().await.unwrap(), 15);
    }

    #[tokio::test]
    async fn fiber_map_propagates_failure() {
      let handle = FiberHandle::completed(FiberId::fresh(), Err::<u8, &str>("oops"));
      let mapped = handle.map(|v: u8| v + 1);
      assert!(mapped.join().await.is_err());
    }

    // ── zip_with ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fiber_zip_with_combines_values_with_function() {
      let a = FiberHandle::<u8, ()>::completed(FiberId::fresh(), Ok(3));
      let b = FiberHandle::<u8, ()>::completed(FiberId::fresh(), Ok(4));
      let combined = a.zip_with(b, |x, y| x + y);
      assert_eq!(combined.join().await.unwrap(), 7);
    }

    // ── interrupt_fork ────────────────────────────────────────────────────

    #[tokio::test]
    async fn fiber_interrupt_fork_eventually_interrupts_handle() {
      let id = FiberId::fresh();
      let handle = FiberHandle::<u8, ()>::pending(id);
      run_async(handle.interrupt_fork(), ()).await.expect("fork");
      tokio::time::sleep(Duration::from_millis(20)).await;
      assert!(handle.is_done());
    }

    // ── fiber_all ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fiber_all_collects_all_successful_results() {
      let handles = vec![
        FiberHandle::completed(FiberId::fresh(), Ok::<u8, ()>(1)),
        FiberHandle::completed(FiberId::fresh(), Ok(2)),
        FiberHandle::completed(FiberId::fresh(), Ok(3)),
      ];
      let result = run_async(fiber_all(handles), ())
        .await
        .expect("fiber_all infallible");
      assert_eq!(result, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn fiber_all_fails_on_first_failure() {
      let handles = vec![
        FiberHandle::completed(FiberId::fresh(), Ok::<u8, &str>(1)),
        FiberHandle::completed(FiberId::fresh(), Err("boom")),
        FiberHandle::completed(FiberId::fresh(), Ok(3)),
      ];
      let result = run_async(fiber_all(handles), ()).await;
      assert!(result.is_err());
    }

    // ── interrupt_all ─────────────────────────────────────────────────────

    #[test]
    fn interrupt_all_interrupts_all_pending_handles() {
      let h1 = FiberHandle::<u8, ()>::pending(FiberId::fresh());
      let h2 = FiberHandle::<u8, ()>::pending(FiberId::fresh());
      run_blocking(interrupt_all(vec![h1.clone(), h2.clone()]), ())
        .expect("interrupt_all infallible");
      assert!(h1.is_done());
      assert!(h2.is_done());
    }

    // ── fiber_succeed / fiber_never ───────────────────────────────────────

    #[test]
    fn fiber_succeed_creates_completed_handle_with_given_value() {
      let id = FiberId::fresh();
      let h: FiberHandle<u8, ()> = fiber_succeed(id, 42);
      assert_eq!(h.id(), id);
      assert_eq!(h.status(), FiberStatus::Succeeded);
      assert_eq!(h.poll_result(), Some(Ok(42)));
    }

    #[test]
    fn fiber_never_creates_pending_handle() {
      let id = FiberId::fresh();
      let h: FiberHandle<u8, ()> = fiber_never(id);
      assert_eq!(h.id(), id);
      assert_eq!(h.status(), FiberStatus::Running);
      assert!(h.poll_result().is_none());
    }
  }
}
