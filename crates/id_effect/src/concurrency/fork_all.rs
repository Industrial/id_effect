//! `fork_all`, `Concurrency` and the `Admission` stub. Filled by `k-fiber-tree`.
//!
//! `fork_all` is the one choke point every parallel combinator goes through (design A §4, ADR
//! 0011 "Parallel combinators"): it runs effects as child fibers of the current fiber and
//! collects their values in input order. On the first failure it interrupts the siblings that
//! are in flight, starts no further ones, waits until every started child has completed (so
//! their finalizers have run), and fails with that first cause.
//!
//! Failure semantics (Effect.ts `Effect.all` with concurrency; ruling of `c-zip-par-all`): the
//! first failure is the result's cause. A sibling that also ends in a genuine failure (a `Fail`
//! or a `Die`, e.g. it failed before it saw the interrupt, or its finalizer died) is combined in
//! with [`Cause::Both`], first failure leftmost. The `Interrupt` leaves of siblings that were
//! interrupted because of that first failure are dropped: they report nothing the caller did
//! not cause. So a lone failure stays exactly that failure.
//!
//! Admission (D6, `c-fabric-scheduler`): `AllFut::start_up_to_limit` is the single point where
//! a child is admitted and spawned. `Bounded(n)` is a purely local window there; only
//! [`Concurrency::Inherit`] awaits an [`AdmissionPermit`] from the ambient
//! [`Fabric`](crate::next::Fabric) before each spawn (asynchronously: a waker, no blocked
//! thread). The permit lives inside the child's program and is dropped when the child exits.
//! See [`crate::next::compute`] for how the ambient fabric is found and why a fiber that
//! already holds a permit may lend it to one child at a time (no nested-admission deadlock).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use crate::failure::Cause;
use crate::next::compute::{Ambient, Fabric};
use crate::next::kernel::{AnyBox, AnyEnv, AnyExit, Effect, Instr, erase, succeed, unbox};
use crate::next::runtime::{BoxFuture, FiberCell, FiberContext, spawn_fiber};

/// How [`fork_all`] schedules its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Concurrency {
  /// One after the other on the current fiber (a left fold; nothing is forked).
  Sequential,
  /// At most `n` children in flight at a time (a sliding window; `0` counts as `1`).
  Bounded(usize),
  /// Every child at once.
  Unbounded,
  /// Let the ambient compute [`Fabric`](crate::next::Fabric) decide (D6; Effect.ts
  /// `concurrency: "inherit"`): every child first awaits one admission permit from the fabric
  /// the current fiber inherited ([`Fabric::provide`](crate::next::Fabric::provide)). With no
  /// ambient fabric this behaves as [`Concurrency::Unbounded`].
  Inherit,
}

/// The compute-fabric admission hook (D6, ADR 0007 Amendment 1): what a
/// [`Fabric`](crate::next::Fabric) consults before [`fork_all`] starts a child under
/// [`Concurrency::Inherit`].
pub trait Admission: Send + Sync {
  /// Resolves once work of `weight` units may start, with the permit that work holds; dropping
  /// the permit gives the capacity back. Must not block a thread while throttled, and dropping
  /// the future before it resolves must take nothing (an interrupted waiter leaks no permit).
  fn admit(&self, weight: u32) -> BoxFuture<'static, AdmissionPermit>;
}

/// Capacity granted by an [`Admission`]: held for as long as the admitted work runs, released
/// on drop.
pub struct AdmissionPermit {
  _guard: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl AdmissionPermit {
  /// A permit whose release is dropping `guard`.
  pub fn new(guard: impl Send + Sync + 'static) -> Self {
    Self {
      _guard: Some(Box::new(guard)),
    }
  }

  /// A permit that holds nothing (an admission without capacity accounting).
  pub fn unlimited() -> Self {
    Self { _guard: None }
  }
}

impl std::fmt::Debug for AdmissionPermit {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AdmissionPermit").finish_non_exhaustive()
  }
}

/// Runs every effect as a child fiber of the current one and collects the values in input
/// order. The first failure interrupts the in-flight siblings, waits for them, and fails with
/// that cause (try-join), plus any genuine sibling failure as [`Cause::Both`] (see the module
/// docs). See [`Concurrency`] for the scheduling modes.
pub fn fork_all<A, E, R>(effects: Vec<Effect<A, E, R>>, c: Concurrency) -> Effect<Vec<A>, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let limit = match c {
    Concurrency::Sequential => {
      return effects.into_iter().fold(succeed(Vec::new()), |acc, e| {
        acc.flat_map(move |mut v| {
          e.map(move |a| {
            v.push(a);
            v
          })
        })
      });
    }
    Concurrency::Bounded(n) => n.max(1),
    Concurrency::Unbounded | Concurrency::Inherit => usize::MAX,
  };
  let instrs: VecDeque<(usize, Instr)> = effects
    .into_iter()
    .map(Effect::into_instr)
    .enumerate()
    .collect();
  let inherit = c == Concurrency::Inherit;
  Effect::from_instr(Instr::FlatMap(
    Box::new(Instr::WithFiber(Box::new(move |env, fiber| {
      let ambient = if inherit { Ambient::of(fiber) } else { None };
      Instr::Future(Box::pin(AllFut::new(
        Arc::clone(fiber),
        Arc::clone(env),
        instrs,
        limit,
        ambient,
      )))
    }))),
    Box::new(|v| {
      let vals: Vec<A> = unbox::<Vec<AnyBox>>(v)
        .into_iter()
        .map(unbox::<A>)
        .collect();
      Instr::Pure(Ok(erase(vals)))
    }),
  ))
}

/// A started child: its context, its exit slot, and the waker that marks it ready.
struct Kid {
  ctx: Arc<FiberContext>,
  cell: FiberCell,
  waker: Waker,
}

/// Which children woke since the last poll, and the waker of the task awaiting `fork_all`.
#[derive(Default)]
struct Ready {
  slots: Mutex<Vec<usize>>,
  parent: Mutex<Option<Waker>>,
}

/// Wakes `fork_all` for one child: records its slot, then wakes the awaiting task. Each poll
/// then visits only the children that woke (no rescan of every pending child).
struct KidWaker {
  slot: usize,
  ready: Arc<Ready>,
}

impl Wake for KidWaker {
  fn wake(self: Arc<Self>) {
    self.wake_by_ref();
  }

  fn wake_by_ref(self: &Arc<Self>) {
    lock(&self.ready.slots).push(self.slot);
    let w = lock(&self.ready.parent).clone();
    if let Some(w) = w {
      w.wake();
    }
  }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts children up to `limit`, awaits them, and on the first failure interrupts the rest.
struct AllFut {
  parent: Arc<FiberContext>,
  env: AnyEnv,
  /// Not yet started, in input order.
  queue: VecDeque<(usize, Instr)>,
  limit: usize,
  /// Started and not yet completed, by input index.
  kids: Vec<Option<Kid>>,
  in_flight: usize,
  ready: Arc<Ready>,
  results: Vec<Option<AnyBox>>,
  failure: Option<Cause<AnyBox>>,
  /// `Inherit` with an ambient fabric: children are admitted through it.
  fabric: Option<Fabric>,
  /// The forking fiber itself holds a permit of `fabric`: it may lend it to one child at a
  /// time (it only waits for its children meanwhile).
  lend: bool,
  /// The child currently running on the lent permit.
  lent: Option<usize>,
  /// The admission being awaited for the next child.
  pending: Option<BoxFuture<'static, AdmissionPermit>>,
}

impl AllFut {
  fn new(
    parent: Arc<FiberContext>,
    env: AnyEnv,
    queue: VecDeque<(usize, Instr)>,
    limit: usize,
    ambient: Option<Ambient>,
  ) -> Self {
    let n = queue.len();
    let (fabric, lend) = match ambient {
      Some(a) => (Some(a.fabric), a.admitted),
      None => (None, false),
    };
    Self {
      parent,
      env,
      queue,
      limit,
      kids: (0..n).map(|_| None).collect(),
      in_flight: 0,
      ready: Arc::new(Ready::default()),
      results: (0..n).map(|_| None).collect(),
      failure: None,
      fabric,
      lend,
      lent: None,
      pending: None,
    }
  }

  /// Admission for the next child (`Inherit` with a fabric only): `Ready(None)` to start it
  /// without a permit of its own, `Ready(Some(p))` once the fabric granted `p`, `Pending` while
  /// the fabric throttles (the admission future has registered `cx`'s waker).
  fn admit_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<AdmissionPermit>> {
    let Some(fabric) = self.fabric.as_ref() else {
      return Poll::Ready(None);
    };
    if self.lend && self.lent.is_none() {
      return Poll::Ready(None);
    }
    let fut = self.pending.get_or_insert_with(|| fabric.admit(1));
    match fut.as_mut().poll(cx) {
      Poll::Ready(p) => {
        self.pending = None;
        Poll::Ready(Some(p))
      }
      Poll::Pending => Poll::Pending,
    }
  }

  /// Starts queued children while fewer than `limit` are in flight (none after a failure).
  /// A started child is marked ready, so its first poll registers its waker.
  ///
  /// This is the one admission point: under `Concurrency::Inherit` with an ambient fabric each
  /// child first awaits an [`AdmissionPermit`] here (`Bounded(n)` stays a local window). The
  /// permit is moved into the child's program and dropped when the child exits.
  fn start_up_to_limit(&mut self, cx: &mut Context<'_>) {
    if self.failure.is_some() {
      self.queue.clear();
      self.pending = None;
      return;
    }
    while self.in_flight < self.limit {
      if self.queue.is_empty() {
        self.pending = None;
        return;
      }
      let Poll::Ready(permit) = self.admit_next(cx) else {
        return;
      };
      let Some((i, instr)) = self.queue.pop_front() else {
        return;
      };
      let ctx = FiberContext::new_child(&self.parent);
      let instr = match (permit, self.fabric.as_ref()) {
        (Some(permit), Some(fabric)) => {
          // The child holds a permit of `fabric`: its own `Inherit` forks may lend it.
          Ambient::mark_admitted(&ctx, fabric);
          Instr::Fold(
            Box::new(instr),
            Box::new(move |exit| {
              drop(permit);
              Instr::Pure(exit)
            }),
          )
        }
        (None, Some(_)) => {
          self.lent = Some(i);
          instr
        }
        (_, None) => instr,
      };
      let cell = spawn_fiber(&ctx, instr, Arc::clone(&self.env));
      let waker = Waker::from(Arc::new(KidWaker {
        slot: i,
        ready: Arc::clone(&self.ready),
      }));
      self.kids[i] = Some(Kid { ctx, cell, waker });
      self.in_flight += 1;
      lock(&self.ready.slots).push(i);
    }
  }

  fn record(&mut self, i: usize, exit: AnyExit) {
    match exit {
      Ok(v) => self.results[i] = Some(v),
      Err(c) => match self.failure.take() {
        None => {
          self.failure = Some(c);
          self.queue.clear();
          for kid in self.kids.iter().flatten() {
            kid.ctx.request_interrupt();
          }
        }
        Some(first) => {
          self.failure = Some(match without_interrupts(c) {
            Some(more) => Cause::Both(Box::new(first), Box::new(more)),
            None => first,
          });
        }
      },
    }
  }
}

/// `c` without its `Interrupt` leaves, or `None` when nothing else is left (see the module docs
/// on failure semantics).
fn without_interrupts<E>(c: Cause<E>) -> Option<Cause<E>> {
  match c {
    Cause::Interrupt(_) => None,
    Cause::Both(l, r) => join(without_interrupts(*l), without_interrupts(*r), Cause::Both),
    Cause::Then(l, r) => join(without_interrupts(*l), without_interrupts(*r), Cause::Then),
    leaf => Some(leaf),
  }
}

fn join<E>(
  l: Option<Cause<E>>,
  r: Option<Cause<E>>,
  both: fn(Box<Cause<E>>, Box<Cause<E>>) -> Cause<E>,
) -> Option<Cause<E>> {
  match (l, r) {
    (Some(l), Some(r)) => Some(both(Box::new(l), Box::new(r))),
    (one, None) | (None, one) => one,
  }
}

impl Future for AllFut {
  type Output = AnyExit;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AnyExit> {
    let this = self.get_mut();
    // Stored before draining the ready list, so a child completing meanwhile still wakes us.
    {
      let mut parent = lock(&this.ready.parent);
      match parent.as_ref() {
        Some(w) if w.will_wake(cx.waker()) => {}
        _ => *parent = Some(cx.waker().clone()),
      }
    }
    loop {
      this.start_up_to_limit(cx);
      let slots = std::mem::take(&mut *lock(&this.ready.slots));
      if slots.is_empty() && !(this.in_flight == 0 && this.queue.is_empty()) {
        return Poll::Pending;
      }
      for i in slots {
        let Some(kid) = this.kids[i].as_ref() else {
          continue;
        };
        let mut kcx = Context::from_waker(&kid.waker);
        if let Poll::Ready(exit) = kid.cell.poll_take(&mut kcx) {
          this.kids[i] = None;
          this.in_flight -= 1;
          if this.lent == Some(i) {
            this.lent = None;
          }
          this.record(i, exit);
        }
      }
      if this.in_flight == 0 && this.queue.is_empty() {
        if let Some(c) = this.failure.take() {
          return Poll::Ready(Err(c));
        }
        let vals: Vec<AnyBox> = this
          .results
          .iter_mut()
          .map(|r| {
            r.take()
              .expect("next: fork_all child result missing (kernel bug)")
          })
          .collect();
        return Poll::Ready(Ok(erase(vals)));
      }
    }
  }
}

impl Drop for AllFut {
  /// Dropped early (the forking fiber was interrupted): interrupt the children still in
  /// flight. The forking fiber's own exit then awaits them as its children.
  fn drop(&mut self) {
    for kid in self.kids.iter().flatten() {
      kid.ctx.request_interrupt();
    }
  }
}
