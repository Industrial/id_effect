//! Concurrency combinators over child fibers (design §4): `zip_par` (fail-fast, try-join
//! semantics), `race`, `timeout`. Each branch is a child fiber with a clone of `R`.
//!
//! Not spiked: `all(.., Concurrency)` and the Fabric `Scheduler` admission hook (D6).

use std::sync::Arc;
use std::time::Duration;

use crate::cause::{Cause, Defect};
use crate::fiber::{self, FiberHandle, Interruptibly, WaitDone, WaitFirst};
use crate::kernel::{Effect, box_future};

fn no_fiber<T, E>() -> Result<T, Cause<E>> {
  Err(Cause::Die(Defect::new(
    "kernel_b: concurrency combinator outside a fiber",
  )))
}

/// Interrupt `h` and wait for it (uninterruptible), discarding its exit.
async fn stop<A, E>(h: &FiberHandle<A, E>) {
  h.state.request_interrupt();
  WaitDone(Arc::clone(&h.state)).await;
}

/// Run `a` and `b` concurrently. The first failure interrupts the sibling.
pub fn zip_par<A, B, E, R>(a: Effect<A, E, R>, b: Effect<B, E, R>) -> Effect<(A, B), E, R>
where
  A: Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Clone + Send + 'static,
{
  Effect::structured(move |r: &mut R| {
    box_future(async move {
      let Some(parent) = fiber::current() else {
        return no_fiber();
      };
      let ha = fiber::spawn_fiber(Arc::clone(&parent.exec), Some(&parent), a, r.clone());
      let hb = fiber::spawn_fiber(Arc::clone(&parent.exec), Some(&parent), b, r.clone());
      let first = match Interruptibly(WaitFirst(Arc::clone(&ha.state), Arc::clone(&hb.state))).await
      {
        Ok(i) => i,
        Err(id) => {
          stop(&ha).await;
          stop(&hb).await;
          return Err(Cause::Interrupt(id));
        }
      };
      if first == 0 {
        match ha.cell.take() {
          Ok(va) => {
            WaitDone(Arc::clone(&hb.state)).await;
            hb.cell.take().map(|vb| (va, vb))
          }
          Err(c) => {
            stop(&hb).await;
            Err(c)
          }
        }
      } else {
        match hb.cell.take() {
          Ok(vb) => {
            WaitDone(Arc::clone(&ha.state)).await;
            ha.cell.take().map(|va| (va, vb))
          }
          Err(c) => {
            stop(&ha).await;
            Err(c)
          }
        }
      }
    })
  })
}

/// First to succeed wins; the loser is interrupted (and awaited, so its finalizers run). If the
/// first to finish fails, the other one decides.
pub fn race<A, E, R>(a: Effect<A, E, R>, b: Effect<A, E, R>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Clone + Send + 'static,
{
  Effect::structured(move |r: &mut R| {
    box_future(async move {
      let Some(parent) = fiber::current() else {
        return no_fiber();
      };
      let ha = fiber::spawn_fiber(Arc::clone(&parent.exec), Some(&parent), a, r.clone());
      let hb = fiber::spawn_fiber(Arc::clone(&parent.exec), Some(&parent), b, r.clone());
      let first = match Interruptibly(WaitFirst(Arc::clone(&ha.state), Arc::clone(&hb.state))).await
      {
        Ok(i) => i,
        Err(id) => {
          stop(&ha).await;
          stop(&hb).await;
          return Err(Cause::Interrupt(id));
        }
      };
      let (win, lose) = if first == 0 { (ha, hb) } else { (hb, ha) };
      match win.cell.take() {
        Ok(v) => {
          stop(&lose).await;
          Ok(v)
        }
        Err(first_err) => {
          WaitDone(Arc::clone(&lose.state)).await;
          lose.cell.take().or(Err(first_err))
        }
      }
    })
  })
}

impl<A: Send + 'static, E: Send + 'static, R: Clone + Send + 'static> Effect<A, E, R> {
  /// `None` if `self` does not finish within `d` (it is then interrupted).
  pub fn timeout(self, d: Duration) -> Effect<Option<A>, E, R> {
    race(self.map(Some), fiber::sleep(d).map(|()| None))
  }

  /// Fail with `on_timeout()` if `self` does not finish within `d`.
  pub fn timeout_fail<F>(self, d: Duration, on_timeout: F) -> Effect<A, E, R>
  where
    F: FnOnce() -> E + Send + 'static,
  {
    self.timeout(d).flat_map(move |o| match o {
      Some(a) => Effect::succeed(a),
      None => Effect::fail(on_timeout()),
    })
  }

  /// Run `self` and `other` concurrently (`zip_par`).
  pub fn zip_par<B: Send + 'static>(self, other: Effect<B, E, R>) -> Effect<(A, B), E, R> {
    zip_par(self, other)
  }
}
