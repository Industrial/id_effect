//! One-shot async rendezvous — mirrors Effect.ts `Deferred<A, E>`.
//!
//! Backed by [`tokio::sync::watch`] with `Option<[`Exit`](crate::Exit)>`: `None` until the first
//! completion, then `Some`. Only the first successful `succeed` / `fail` / … wins; later attempts
//! return `false` from those `Effect<bool, …>` helpers.

use tokio::sync::watch;

use crate::failure::cause::Cause;
use crate::failure::exit::Exit;
use crate::kernel::{Effect, box_future};
use crate::runtime::{FiberId, Never};

// ── Deferred ─────────────────────────────────────────────────────────────────

/// Shared one-shot cell completed with an [`Exit`] value.
#[derive(Clone, Debug)]
pub struct Deferred<A, E> {
  tx: watch::Sender<Option<Exit<A, E>>>,
  rx: watch::Receiver<Option<Exit<A, E>>>,
}

impl<A, E> Deferred<A, E>
where
  A: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
{
  /// Create an incomplete deferred (watch value starts at `None`).
  pub fn make() -> Effect<Deferred<A, E>, Never, ()> {
    Effect::new(|_r| {
      let (tx, rx) = watch::channel(None);
      Ok(Deferred { tx, rx })
    })
  }

  /// Suspend until a completion is stored, then return success or the failure [`Cause`].
  ///
  /// The error channel is [`Cause<E>`] so interrupt / die / structured failures surface faithfully.
  pub fn wait(&self) -> Effect<A, Cause<E>, ()> {
    let mut rx = self.rx.clone();
    Effect::new_async(move |_r| {
      box_future(async move {
        loop {
          if let Some(exit) = rx.borrow().clone() {
            return match exit {
              Exit::Success(a) => Ok(a),
              Exit::Failure(c) => Err(c),
            };
          }
          if rx.changed().await.is_err() {
            return Err(Cause::die("deferred: all senders dropped"));
          }
        }
      })
    })
  }

  /// Like [`Self::wait`], but returns a [`Send`] future for use inside [`tokio::spawn`] and other
  /// multi-thread executors (the boxed [`Effect`] future from [`Effect::run`] is not [`Send`]).
  pub fn wait_future(
    &self,
  ) -> impl std::future::Future<Output = Result<A, Cause<E>>> + Send + 'static
  where
    A: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
  {
    let mut rx = self.rx.clone();
    async move {
      loop {
        if let Some(exit) = rx.borrow().clone() {
          return match exit {
            Exit::Success(a) => Ok(a),
            Exit::Failure(c) => Err(c),
          };
        }
        if rx.changed().await.is_err() {
          return Err(Cause::die("deferred: all senders dropped"));
        }
      }
    }
  }

  /// Try to complete with success (non-blocking). Returns whether this was the first completion.
  #[inline]
  pub fn try_succeed(&self, value: A) -> bool
  where
    A: Clone,
  {
    self.tx.send_if_modified(|slot| {
      if slot.is_none() {
        *slot = Some(Exit::succeed(value.clone()));
        true
      } else {
        false
      }
    })
  }

  /// Try to complete with a failure [`Cause`] (non-blocking).
  #[inline]
  pub fn try_fail_cause(&self, cause: Cause<E>) -> bool
  where
    E: Clone,
  {
    self.tx.send_if_modified(|slot| {
      if slot.is_none() {
        *slot = Some(Exit::Failure(cause.clone()));
        true
      } else {
        false
      }
    })
  }

  /// Non-blocking read of the current exit, if any.
  pub fn poll(&self) -> Effect<Option<Exit<A, E>>, Never, ()> {
    let rx = self.rx.clone();
    Effect::new(move |_r| Ok(rx.borrow().clone()))
  }

  /// Returns whether this deferred has been completed (watch value is `Some`).
  pub fn is_done(&self) -> Effect<bool, Never, ()> {
    let rx = self.rx.clone();
    Effect::new(move |_r| Ok(rx.borrow().is_some()))
  }

  /// Complete with success. Returns `true` if this was the first completion.
  pub fn succeed(&self, value: A) -> Effect<bool, Never, ()> {
    let tx = self.tx.clone();
    Effect::new(move |_r| {
      Ok(tx.send_if_modified(|slot| {
        if slot.is_none() {
          *slot = Some(Exit::succeed(value.clone()));
          true
        } else {
          false
        }
      }))
    })
  }

  /// Complete with [`Exit::fail`]. Returns `true` if this was the first completion.
  pub fn fail(&self, error: E) -> Effect<bool, Never, ()> {
    let tx = self.tx.clone();
    Effect::new(move |_r| {
      Ok(tx.send_if_modified(|slot| {
        if slot.is_none() {
          *slot = Some(Exit::fail(error.clone()));
          true
        } else {
          false
        }
      }))
    })
  }

  /// Complete with an arbitrary [`Cause`]. Returns `true` if this was the first completion.
  pub fn fail_cause(&self, cause: Cause<E>) -> Effect<bool, Never, ()> {
    let tx = self.tx.clone();
    Effect::new(move |_r| {
      Ok(tx.send_if_modified(|slot| {
        if slot.is_none() {
          *slot = Some(Exit::Failure(cause.clone()));
          true
        } else {
          false
        }
      }))
    })
  }

  /// Complete with [`Exit::interrupt`]. Returns `true` if this was the first completion.
  pub fn interrupt(&self) -> Effect<bool, Never, ()> {
    let tx = self.tx.clone();
    Effect::new(move |_r| {
      Ok(tx.send_if_modified(|slot| {
        if slot.is_none() {
          *slot = Some(Exit::interrupt(FiberId::fresh()));
          true
        } else {
          false
        }
      }))
    })
  }

  /// Run `eff`; on success, complete with [`Exit::succeed`]. Returns whether this was the first
  /// completion. Effect errors from `eff` are returned as `Err` **without** completing the deferred.
  pub fn complete<R>(&self, eff: Effect<A, E, R>) -> Effect<bool, E, R> {
    let tx = self.tx.clone();
    Effect::new_async(move |r| {
      box_future(async move {
        let value = eff.run(r).await?;
        Ok(tx.send_if_modified(|slot| {
          if slot.is_none() {
            *slot = Some(Exit::succeed(value));
            true
          } else {
            false
          }
        }))
      })
    })
  }

  /// Force the stored exit, overwriting any prior value (use sparingly).
  pub fn unsafe_done(&self, exit: Exit<A, E>) -> Effect<(), Never, ()> {
    let tx = self.tx.clone();
    Effect::new(move |_r| {
      let _ = tx.send(Some(exit));
      Ok(())
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::kernel::succeed;

  #[tokio::test]
  async fn deferred_wait_suspends_until_succeed() {
    let d = Deferred::<u8, ()>::make().run(&mut ()).await.unwrap();
    let d2 = d.clone();
    let (out, _) = tokio::join!(async { d.wait().run(&mut ()).await }, async {
      tokio::task::yield_now().await;
      d2.succeed(7).run(&mut ()).await.unwrap();
    },);
    assert_eq!(out, Ok(7));
  }

  #[tokio::test]
  async fn deferred_second_succeed_returns_false() {
    let d = Deferred::<i32, ()>::make().run(&mut ()).await.unwrap();
    assert!(d.succeed(1).run(&mut ()).await.unwrap());
    assert!(!d.succeed(2).run(&mut ()).await.unwrap());
  }

  #[tokio::test]
  async fn deferred_fail_propagates_error() {
    let d = Deferred::<u8, &str>::make().run(&mut ()).await.unwrap();
    assert!(d.fail("boom").run(&mut ()).await.unwrap());
    let out = d.wait().run(&mut ()).await;
    assert_eq!(out, Err(Cause::fail("boom")));
  }

  #[tokio::test]
  async fn deferred_interrupt_delivers_interrupt_cause() {
    let d = Deferred::<u8, ()>::make().run(&mut ()).await.unwrap();
    assert!(d.interrupt().run(&mut ()).await.unwrap());
    let out = d.wait().run(&mut ()).await;
    match out {
      Err(Cause::Interrupt(_)) => {}
      o => panic!("expected interrupt cause, got {o:?}"),
    }
  }

  #[tokio::test]
  async fn deferred_poll_none_before_complete() {
    let d = Deferred::<u16, ()>::make().run(&mut ()).await.unwrap();
    assert_eq!(d.poll().run(&mut ()).await.unwrap(), None);
    d.succeed(42).run(&mut ()).await.unwrap();
    assert_eq!(
      d.poll().run(&mut ()).await.unwrap(),
      Some(Exit::succeed(42_u16))
    );
  }

  #[tokio::test]
  async fn deferred_second_fail_cause_returns_false() {
    let d = Deferred::<(), ()>::make().run(&mut ()).await.unwrap();
    assert!(d.fail_cause(Cause::die("a")).run(&mut ()).await.unwrap());
    assert!(!d.fail_cause(Cause::die("b")).run(&mut ()).await.unwrap());
  }

  #[tokio::test]
  async fn deferred_complete_runs_effect_and_sets_success() {
    let d = Deferred::<u8, ()>::make().run(&mut ()).await.unwrap();
    let eff = succeed::<u8, (), ()>(9);
    assert!(d.complete(eff).run(&mut ()).await.unwrap());
    assert_eq!(d.wait().run(&mut ()).await, Ok(9));
  }

  #[tokio::test]
  async fn deferred_unsafe_done_overwrites_prior() {
    let d = Deferred::<u8, ()>::make().run(&mut ()).await.unwrap();
    d.succeed(1).run(&mut ()).await.unwrap();
    d.unsafe_done(Exit::succeed(2)).run(&mut ()).await.unwrap();
    assert_eq!(d.wait().run(&mut ()).await, Ok(2));
  }
}
