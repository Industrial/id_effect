//! `fork_all`, `Concurrency` and the `Admission` stub. Filled by `k-fiber-tree`.
//!
//! `fork_all` is the one choke point every parallel combinator goes through (design A §4, ADR
//! 0011 "Parallel combinators"): it runs effects as child fibers of the current fiber and
//! collects their values in input order. On the first failure it interrupts the siblings that
//! are in flight, starts no further ones, waits until every started child has completed (so
//! their finalizers have run), and fails with that first cause.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use crate::failure::Cause;
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
  /// Let the compute fabric decide (D6). Until `c-fabric-scheduler` wires [`Admission`], this
  /// behaves as [`Concurrency::Unbounded`].
  Inherit,
}

/// The compute-fabric admission hook (D6). Declared here, not yet consulted by [`fork_all`]
/// (k-rebuild L11; wiring is `c-fabric-scheduler`'s).
pub trait Admission: Send + Sync {
  /// Resolves once work of `weight` units may start.
  fn admit(&self, weight: u32) -> BoxFuture<'static, ()>;
}

/// Runs every effect as a child fiber of the current one and collects the values in input
/// order. The first failure interrupts the in-flight siblings, waits for them, and fails with
/// that cause (try-join). See [`Concurrency`] for the scheduling modes.
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
  Effect::from_instr(Instr::FlatMap(
    Box::new(Instr::WithFiber(Box::new(move |env, fiber| {
      Instr::Future(Box::pin(AllFut::new(
        Arc::clone(fiber),
        Arc::clone(env),
        instrs,
        limit,
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
}

impl AllFut {
  fn new(
    parent: Arc<FiberContext>,
    env: AnyEnv,
    queue: VecDeque<(usize, Instr)>,
    limit: usize,
  ) -> Self {
    let n = queue.len();
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
    }
  }

  /// Starts queued children while fewer than `limit` are in flight (none after a failure).
  /// A started child is marked ready, so its first poll registers its waker.
  fn start_up_to_limit(&mut self) {
    if self.failure.is_some() {
      self.queue.clear();
      return;
    }
    while self.in_flight < self.limit {
      let Some((i, instr)) = self.queue.pop_front() else {
        return;
      };
      let ctx = FiberContext::new_child(&self.parent);
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
      Err(c) => {
        if self.failure.is_none() {
          self.failure = Some(c);
          self.queue.clear();
          for kid in self.kids.iter().flatten() {
            kid.ctx.request_interrupt();
          }
        }
      }
    }
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
      this.start_up_to_limit();
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
