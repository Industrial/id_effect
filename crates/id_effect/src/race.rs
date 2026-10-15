//! Racing and timeouts on fibers: [`Effect::race`], [`race_all`], [`Effect::race_first`],
//! [`Effect::timeout`] and [`Effect::timeout_fail`]. Filled by `c-race-timeout`.
//!
//! Every racer runs as a child fiber of the racing fiber (so the racer's interruption reaches
//! them, and its exit awaits them). Semantics follow Effect.ts:
//!
//! - **`race` / `race_all`** (`Effect.race`, `Effect.raceAll`): the first *success* wins. The
//!   other racers are interrupted and awaited, so their finalizers have run before the result is
//!   returned. A failure does not end the race; when every racer fails, the race fails with all
//!   the causes combined by `Cause::Both` in input order (`race(a, b)` fails with
//!   `Both(cause_a, cause_b)`, like Effect.ts `Cause.parallel`). Effect.ts `raceAll` keeps only
//!   the last failure; here `race_all` keeps every one, so `race_all([a, b])` and `a.race(b)`
//!   agree.
//! - **`race_first`** (`Effect.raceFirst`): the first racer to *complete* wins, success or
//!   failure, and the other is interrupted and awaited.
//! - **`timeout`** (`Effect.timeout`) is `race_first` against a sleep on the fiber's executor
//!   clock (virtual time on `TestExecutor`). The Rust shape returns `Option<A>`: `Some(a)` when
//!   the effect finished first, `None` when the deadline passed. The Effect.ts 3.x
//!   `TimeoutException` error channel is the separate [`Effect::timeout_fail`], which fails with
//!   an error built by the caller instead. A failure of the effect before the deadline is
//!   re-raised as is. Because the interrupted racer is awaited, an uninterruptible (masked)
//!   effect delays the timeout until it completes; the result is still `None`.
//!
//! `race_all` of no effects dies (as Effect.ts `raceAll` of an empty iterable does).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use crate::failure::Cause;
use crate::kernel::{AnyBox, AnyExit, Effect, Instr, die, fail, sleep, succeed};
use crate::runtime::{FiberCell, FiberContext, spawn_fiber};

/// Which outcome ends a race.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
  /// The first success wins; failures are collected (`race`, `race_all`).
  FirstSuccess,
  /// The first completion wins, success or failure (`race_first`, `timeout`).
  FirstExit,
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Races this effect against `that` (Effect.ts `Effect.race`): the first success wins, and the
  /// loser is interrupted and awaited (its finalizers have run) before the result is returned.
  /// When both fail, the race fails with `Cause::Both(self_cause, that_cause)`.
  ///
  /// ```
  /// use std::time::Duration;
  ///
  /// use id_effect::Exit;
  /// use id_effect::{Effect, sleep, succeed};
  ///
  /// let slow: Effect<&str> = sleep(Duration::from_secs(3600)).map(|()| "slow");
  /// let fast: Effect<&str> = succeed("fast");
  /// assert_eq!(slow.race(fast).block_on(), Exit::succeed("fast"));
  /// ```
  pub fn race(self, that: Effect<A, E, R>) -> Effect<A, E, R> {
    race_with(vec![self, that], Mode::FirstSuccess)
  }

  /// Races this effect against `that` (Effect.ts `Effect.raceFirst`): the first to complete
  /// wins, whether it succeeded or failed, and the other is interrupted and awaited.
  pub fn race_first(self, that: Effect<A, E, R>) -> Effect<A, E, R> {
    race_with(vec![self, that], Mode::FirstExit)
  }

  /// Bounds this effect by `d` on the fiber's executor clock (Effect.ts `Effect.timeout`):
  /// `Some(a)` when it finished in time, `None` once `d` has passed (the effect is then
  /// interrupted and awaited, so a masked effect delays the `None`). A failure before the
  /// deadline is re-raised.
  ///
  /// On a [`TestExecutor`](crate::TestExecutor) the deadline is virtual time:
  ///
  /// ```
  /// use std::sync::Arc;
  /// use std::time::Duration;
  ///
  /// use id_effect::{Effect, TestExecutor, sleep};
  ///
  /// let exec = TestExecutor::new();
  /// let slow: Effect<u8> = sleep(Duration::from_secs(60)).map(|()| 1);
  /// let h = slow
  ///   .timeout(Duration::from_secs(10))
  ///   .fork_root(Arc::new(exec.clone()));
  /// exec.advance(Duration::from_secs(9));
  /// assert!(!h.is_done());
  /// exec.advance(Duration::from_secs(1));
  /// assert!(h.is_done(), "timed out at virtual 10 s");
  /// ```
  pub fn timeout(self, d: Duration) -> Effect<Option<A>, E, R> {
    let timer: Effect<Option<A>, E, R> = sleep(d).map(|()| None);
    race_with(vec![self.map(Some), timer], Mode::FirstExit)
  }

  /// Like [`timeout`](Effect::timeout), but fails with `on_timeout()` once `d` has passed
  /// (Effect.ts `Effect.timeoutFail`).
  pub fn timeout_fail(
    self,
    d: Duration,
    on_timeout: impl FnOnce() -> E + Send + 'static,
  ) -> Effect<A, E, R> {
    self.timeout(d).flat_map(move |o| match o {
      Some(a) => succeed(a),
      None => fail(on_timeout()),
    })
  }
}

/// Races every effect (Effect.ts `Effect.raceAll`): the first success wins, and the others are
/// interrupted and awaited (their finalizers have run) before the result is returned. When every
/// effect fails, the race fails with all the causes combined by `Cause::Both` in input order.
/// An empty input dies.
pub fn race_all<A, E, R>(effects: impl IntoIterator<Item = Effect<A, E, R>>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let effects: Vec<_> = effects.into_iter().collect();
  if effects.is_empty() {
    return die("id_effect: race_all of no effects");
  }
  race_with(effects, Mode::FirstSuccess)
}

/// Forks every effect as a child of the current fiber when the step runs, and awaits the race.
fn race_with<A, E, R>(effects: Vec<Effect<A, E, R>>, mode: Mode) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let instrs: Vec<Instr> = effects.into_iter().map(Effect::into_instr).collect();
  Effect::from_instr(Instr::WithFiber(Box::new(move |env, fiber| {
    let ready = Arc::new(Ready::default());
    let kids: Vec<Option<Kid>> = instrs
      .into_iter()
      .enumerate()
      .map(|(i, instr)| {
        let ctx = FiberContext::new_child(fiber);
        let cell = spawn_fiber(&ctx, instr, Arc::clone(env));
        let waker = Waker::from(Arc::new(KidWaker {
          slot: i,
          ready: Arc::clone(&ready),
        }));
        // Marked ready, so the first poll registers the kid's waker.
        lock(&ready.slots).push(i);
        Some(Kid { ctx, cell, waker })
      })
      .collect();
    let n = kids.len();
    Instr::Future(Box::pin(RaceFut {
      in_flight: n,
      kids,
      ready,
      mode,
      winner: None,
      failures: (0..n).map(|_| None).collect(),
    }))
  })))
}

/// A started racer: its context, its exit slot, and the waker that marks it ready.
struct Kid {
  ctx: Arc<FiberContext>,
  cell: FiberCell,
  waker: Waker,
}

/// Which racers woke since the last poll, and the waker of the task awaiting the race.
#[derive(Default)]
struct Ready {
  slots: Mutex<Vec<usize>>,
  parent: Mutex<Option<Waker>>,
}

/// Wakes the race for one racer: records its slot, then wakes the awaiting task.
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

/// Awaits the racers: once a winner is decided the rest are interrupted, and the race completes
/// only when every racer has completed.
struct RaceFut {
  /// Racers not yet completed, by input index.
  kids: Vec<Option<Kid>>,
  in_flight: usize,
  ready: Arc<Ready>,
  mode: Mode,
  winner: Option<AnyExit>,
  /// Failure causes by input index (`Mode::FirstSuccess`, until a winner is decided).
  failures: Vec<Option<Cause<AnyBox>>>,
}

impl RaceFut {
  fn record(&mut self, i: usize, exit: AnyExit) {
    if self.winner.is_some() {
      // A loser finishing after the decision (usually interrupted): its outcome is dropped.
      return;
    }
    match (self.mode, exit) {
      (Mode::FirstSuccess, Err(c)) => self.failures[i] = Some(c),
      (_, exit) => {
        self.winner = Some(exit);
        self.failures.clear();
        for kid in self.kids.iter().flatten() {
          kid.ctx.request_interrupt();
        }
      }
    }
  }

  /// The race's exit once every racer has completed.
  fn finish(&mut self) -> AnyExit {
    if let Some(exit) = self.winner.take() {
      return exit;
    }
    let cause = std::mem::take(&mut self.failures)
      .into_iter()
      .flatten()
      .reduce(Cause::both)
      .expect("id_effect: race with no racers (kernel bug)");
    Err(cause)
  }
}

impl Future for RaceFut {
  type Output = AnyExit;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AnyExit> {
    let this = self.get_mut();
    // Stored before draining the ready list, so a racer completing meanwhile still wakes us.
    {
      let mut parent = lock(&this.ready.parent);
      match parent.as_ref() {
        Some(w) if w.will_wake(cx.waker()) => {}
        _ => *parent = Some(cx.waker().clone()),
      }
    }
    loop {
      if this.in_flight == 0 {
        return Poll::Ready(this.finish());
      }
      let slots = std::mem::take(&mut *lock(&this.ready.slots));
      if slots.is_empty() {
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
    }
  }
}

impl Drop for RaceFut {
  /// Dropped early (the racing fiber was interrupted): interrupt the racers still running. The
  /// racing fiber's own exit then awaits them as its children.
  fn drop(&mut self) {
    for kid in self.kids.iter().flatten() {
      kid.ctx.request_interrupt();
    }
  }
}
