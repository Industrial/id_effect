//! `Scope` (finalizer registry), `Effect::scoped` and `Scope::fork_in`. Filled by
//! `k-scope-fiberref`.
//!
//! A [`Scope`] collects finalizers (`Effect<(), Never, HNil>`, optionally fed the closing
//! [`ExitKind`]) and runs them once, uninterruptibly and in reverse order of registration, when
//! it is closed. [`Scope::fork_in`] ties a fiber to the scope instead of to the forking fiber:
//! closing the scope interrupts the fiber and awaits it (its own finalizers and children
//! included), at the point in the LIFO order where it was forked. [`Effect::scoped`] opens a
//! scope for one effect and closes it however the effect ends.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use crate::failure::Cause;
use crate::next::capability::HNil;
use crate::next::kernel::{AnyBox, AnyEnv, AnyExit, Effect, ExitKind, Instr, MaskOp, erase};
use crate::next::runtime::{Done, FiberContext, FiberHandle, spawn_fiber};
use crate::runtime::Never;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A registered finalizer, fed the closing [`ExitKind`].
type Finalizer = Box<dyn FnOnce(ExitKind) -> Effect<(), Never, HNil> + Send>;

enum State {
  /// Finalizers by registration key; keys only grow, so reverse key order is LIFO.
  Open {
    next: u64,
    fins: BTreeMap<u64, Finalizer>,
  },
  Closed(ExitKind),
}

struct Inner {
  state: Mutex<State>,
}

impl Inner {
  /// Registers `fin`, or hands it back with the closing kind when the scope is closed.
  fn register(&self, fin: Finalizer) -> Result<u64, (Finalizer, ExitKind)> {
    match &mut *lock(&self.state) {
      State::Open { next, fins } => {
        let key = *next;
        *next += 1;
        fins.insert(key, fin);
        Ok(key)
      }
      State::Closed(kind) => Err((fin, *kind)),
    }
  }

  fn unregister(&self, key: u64) {
    if let State::Open { fins, .. } = &mut *lock(&self.state) {
      fins.remove(&key);
    }
  }

  /// Closes the scope, returning its finalizers in registration order (`None` when it was
  /// already closed).
  fn take(&self, kind: ExitKind) -> Option<Vec<Finalizer>> {
    let mut st = lock(&self.state);
    match std::mem::replace(&mut *st, State::Closed(kind)) {
      State::Open { fins, .. } => Some(fins.into_values().collect()),
      State::Closed(k) => {
        *st = State::Closed(k);
        None
      }
    }
  }
}

/// A finalizer registry that outlives a single effect (Effect.ts `Scope`).
///
/// Cloning a `Scope` clones the handle: every clone registers into and closes the same scope.
#[derive(Clone)]
pub struct Scope {
  inner: Arc<Inner>,
}

impl std::fmt::Debug for Scope {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match &*lock(&self.inner.state) {
      State::Open { fins, .. } => f
        .debug_struct("Scope")
        .field("open", &true)
        .field("finalizers", &fins.len())
        .finish(),
      State::Closed(kind) => f.debug_struct("Scope").field("closed", kind).finish(),
    }
  }
}

impl Default for Scope {
  fn default() -> Self {
    Self::new()
  }
}

/// Runs `fins` last-registered first, each uninterruptibly (the caller masks) and each even
/// when an earlier one died; the defects are sequenced with `Cause::Then`.
fn run_finalizers(mut fins: Vec<Finalizer>, kind: ExitKind, acc: Option<Cause<AnyBox>>) -> Instr {
  match fins.pop() {
    None => Instr::Pure(match acc {
      None => Ok(erase(())),
      Some(c) => Err(c),
    }),
    Some(fin) => Instr::Fold(
      // `fin` is a user closure: called inside a `Sync` step so a panic becomes `Die`.
      Box::new(Instr::Sync(Box::new(move |_| fin(kind).into_instr()))),
      Box::new(move |exit| {
        let acc = match (acc, exit) {
          (acc, Ok(_)) => acc,
          (None, Err(c)) => Some(c),
          (Some(a), Err(c)) => Some(Cause::then(a, c)),
        };
        run_finalizers(fins, kind, acc)
      }),
    ),
  }
}

/// Runs an `HNil` instruction under any env (finalizers and `close` need no capabilities).
fn under_hnil(i: Instr) -> Instr {
  Instr::Provide(Box::new(i), Box::new(|_| Arc::new(HNil) as AnyEnv))
}

/// Runs `fin` uninterruptibly, then re-raises `exit` (a dying `fin` is sequenced after it).
fn then_finalize(fin: Instr, exit: AnyExit) -> Instr {
  Instr::Fold(
    Box::new(fin),
    Box::new(move |fin_exit| match fin_exit {
      Ok(_) => Instr::Pure(exit),
      Err(fc) => Instr::Pure(Err(match exit {
        Ok(_) => fc,
        Err(c) => Cause::then(c, fc),
      })),
    }),
  )
}

impl Scope {
  /// A new, open scope. A plain constructor: building a scope has no effect.
  pub fn new() -> Self {
    Self {
      inner: Arc::new(Inner {
        state: Mutex::new(State::Open {
          next: 0,
          fins: BTreeMap::new(),
        }),
      }),
    }
  }

  /// `true` once [`close`](Self::close) has run.
  pub fn is_closed(&self) -> bool {
    matches!(*lock(&self.inner.state), State::Closed(_))
  }

  /// The [`ExitKind`] the scope was closed with, if it is closed.
  pub fn closed_with(&self) -> Option<ExitKind> {
    match *lock(&self.inner.state) {
      State::Closed(kind) => Some(kind),
      State::Open { .. } => None,
    }
  }

  /// Registers `fin` to run when the scope closes (see [`add_finalizer_exit`]).
  ///
  /// [`add_finalizer_exit`]: Self::add_finalizer_exit
  pub fn add_finalizer<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
    fin: Effect<(), Never, HNil>,
  ) -> Effect<(), E, R> {
    self.add_finalizer_exit(move |_| fin)
  }

  /// Registers `fin`, fed the closing [`ExitKind`], to run when the scope closes.
  ///
  /// Registration happens when the returned effect runs. When the scope is already closed,
  /// `fin` runs at once (uninterruptibly, with the kind the scope was closed with); a defect in
  /// it is raised by the returned effect.
  pub fn add_finalizer_exit<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
    fin: impl FnOnce(ExitKind) -> Effect<(), Never, HNil> + Send + 'static,
  ) -> Effect<(), E, R> {
    let inner = Arc::clone(&self.inner);
    Effect::from_instr(Instr::Sync(Box::new(move |_| {
      match inner.register(Box::new(fin)) {
        Ok(_) => Instr::Pure(Ok(erase(()))),
        Err((fin, kind)) => Instr::Mask(
          Box::new(under_hnil(run_finalizers(vec![fin], kind, None))),
          MaskOp::Inc,
        ),
      }
    })))
  }

  /// Closes the scope with `kind`: runs every finalizer in reverse order of registration,
  /// exactly once and uninterruptibly, interrupting and awaiting the fibers forked into it.
  ///
  /// Every finalizer runs even when an earlier one dies; the defects are raised together
  /// (`Cause::Then`). Closing an already closed scope is a no-op.
  pub fn close(&self, kind: ExitKind) -> Effect<(), Never, HNil> {
    let inner = Arc::clone(&self.inner);
    Effect::from_instr(Instr::Mask(
      Box::new(Instr::Sync(Box::new(move |_| match inner.take(kind) {
        Some(fins) => run_finalizers(fins, kind, None),
        None => Instr::Pure(Ok(erase(()))),
      }))),
      MaskOp::Inc,
    ))
  }

  /// Forks `eff` as a fiber owned by this scope rather than by the forking fiber (Effect.ts
  /// `forkIn`).
  ///
  /// The fiber starts with the forking fiber's env and fiber refs. It is not interrupted when
  /// the forking fiber exits; it is interrupted and awaited when the scope closes. A fiber that
  /// completes earlier unregisters itself. Forking into a closed scope yields a fiber that is
  /// interrupted before it runs.
  pub fn fork_in<A, E, R>(&self, eff: Effect<A, E, R>) -> Effect<FiberHandle<A, E>, Never, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    Effect::from_instr(Instr::WithFiber(Box::new(move |env, fiber| {
      let child = FiberContext::new_detached(fiber);
      let stop = {
        let child = Arc::clone(&child);
        Box::new(move |_: ExitKind| {
          Effect::from_instr(Instr::Future(Box::pin(async move {
            child.request_interrupt();
            Done(child).await;
            Ok(erase(()))
          })))
        }) as Finalizer
      };
      let instr = match inner.register(stop) {
        Ok(key) => {
          let weak: Weak<Inner> = Arc::downgrade(&inner);
          Instr::Fold(
            Box::new(eff.into_instr()),
            Box::new(move |exit| {
              if let Some(inner) = weak.upgrade() {
                inner.unregister(key);
              }
              Instr::Pure(exit)
            }),
          )
        }
        Err(_) => {
          child.request_interrupt();
          eff.into_instr()
        }
      };
      let cell = spawn_fiber(&child, instr, Arc::clone(env));
      Instr::Pure(Ok(erase(FiberHandle::<A, E>::new(child, cell))))
    })))
  }
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Runs `f`'s effect with a fresh [`Scope`] and closes the scope however the effect ends
  /// (success, failure, defect or interrupt), with the matching [`ExitKind`].
  ///
  /// The body keeps the caller's interruptibility; closing is uninterruptible. A defect in a
  /// finalizer is sequenced after the body's outcome (`Cause::Then`), or replaces a success.
  pub fn scoped(f: impl FnOnce(Scope) -> Effect<A, E, R> + Send + 'static) -> Self {
    Self::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let m = fiber.mask();
      let scope = Scope::new();
      let body = scope.clone();
      Instr::Mask(
        Box::new(Instr::Fold(
          Box::new(Instr::Mask(
            // `f` is a user closure: called inside a `Sync` step so a panic becomes `Die` and
            // the scope still closes.
            Box::new(Instr::Sync(Box::new(move |_| f(body).into_instr()))),
            MaskOp::Set(m),
          )),
          Box::new(move |exit| {
            let kind = ExitKind::of_result(&exit);
            then_finalize(under_hnil(scope.close(kind).into_instr()), exit)
          }),
        )),
        MaskOp::Inc,
      )
    })))
  }
}
