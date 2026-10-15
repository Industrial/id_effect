//! Fiber supervision: [`supervised`] restart loops under a [`SupervisorPolicy`], the
//! [`Supervisor`] that owns supervised fibers, and the pure [`RestartIntensity`] limiter.
//!
//! 0.6 stopped a supervised loop cooperatively through a `CancellationToken` checked between
//! attempts. In 0.7 a supervised loop is an ordinary fiber: [`Supervisor::shutdown`]
//! closes the supervisor's [`Scope`], which interrupts every fiber forked into it, wherever it is
//! suspended (inside an attempt or between two). Interruption is never restarted.
//!
//! The 0.6 policies took a `Schedule`; the restart policies here take a fixed `delay` between
//! attempts (wrap the effect in [`Effect::retry`] when it needs a [`Schedule`](crate::Schedule)).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::waiters::{fail_cause, hnil_in, lock};
use crate::failure::Never;
use crate::failure::{Cause, Exit};
use crate::kernel::{Effect, ExitKind, sleep, succeed, suspend, yield_fiber};
use crate::runtime::FiberHandle;
use crate::scope::Scope;

/// A sliding-window restart-intensity limiter (OTP `max_restarts` within `within`).
///
/// [`note`](Self::note) records a restart at `now`, evicts timestamps older than the window and
/// returns `true` while the retained count stays at or below `max_restarts`. Pure and
/// clock-agnostic: the caller supplies `now`.
#[derive(Clone, Debug)]
pub struct RestartIntensity {
  max_restarts: u32,
  within: Duration,
  events: VecDeque<Instant>,
}

impl RestartIntensity {
  /// A limiter permitting at most `max_restarts` within `within`.
  pub fn new(max_restarts: u32, within: Duration) -> Self {
    Self {
      max_restarts,
      within,
      events: VecDeque::new(),
    }
  }

  /// Records a restart at `now`; `false` once the restarts inside the window exceed
  /// `max_restarts` (the caller should escalate instead of restarting).
  pub fn note(&mut self, now: Instant) -> bool {
    self.events.push_back(now);
    let window_start = now.checked_sub(self.within).unwrap_or(now);
    while self.events.front().is_some_and(|t| *t < window_start) {
      self.events.pop_front();
    }
    self.events.len() <= self.max_restarts as usize
  }

  /// The maximum number of restarts within the window.
  pub fn max_restarts(&self) -> u32 {
    self.max_restarts
  }

  /// The sliding window.
  pub fn within(&self) -> Duration {
    self.within
  }
}

/// What [`supervised`] does when an attempt fails (typed failure or defect; an interrupt always
/// propagates).
#[derive(Clone, Debug)]
pub enum SupervisorPolicy<A = ()> {
  /// Run once; propagate the first failure.
  Terminate,
  /// Restart after `delay` on every failure until an attempt succeeds.
  Restart {
    /// The pause between attempts (`Duration::ZERO` yields the fiber once instead).
    delay: Duration,
  },
  /// Like [`Restart`](Self::Restart), but after `limit` retries the loop fails with every
  /// failure so far, sequenced with `Cause::Then`. `limit == 0` fails on the first failure.
  RestartWithLimit {
    /// The number of retries after a failure (the first run is not counted).
    limit: u64,
    /// The pause between attempts.
    delay: Duration,
  },
  /// Propagate the first failure (the same as [`Terminate`](Self::Terminate) for one child; kept
  /// for parity with the Effect.ts naming).
  Escalate,
  /// On failure, succeed with `recover` instead.
  Ignore {
    /// The value substituted for a failed attempt.
    recover: A,
  },
}

type Make<A, E, R> = Arc<Mutex<dyn FnMut() -> Effect<A, E, R> + Send>>;

/// Runs the effects built by `make` under `policy` until the policy completes.
///
/// Each attempt is a fresh `make()`. A failure (typed or defect) is handled by the policy; an
/// interrupt is never restarted, so interrupting the fiber running the loop stops it.
pub fn supervised<A, E, R, F>(policy: SupervisorPolicy<A>, make: F) -> Effect<A, E, R>
where
  A: Clone + Send + Sync + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnMut() -> Effect<A, E, R> + Send + 'static,
{
  attempt(Arc::new(policy), Arc::new(Mutex::new(make)), 0, None)
}

fn attempt<A, E, R>(
  policy: Arc<SupervisorPolicy<A>>,
  make: Make<A, E, R>,
  used: u64,
  acc: Option<Cause<E>>,
) -> Effect<A, E, R>
where
  A: Clone + Send + Sync + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  suspend(move || {
    let eff = (*lock(&make))();
    eff.exit::<E>().flat_map(move |exit| match exit {
      Exit::Success(a) => succeed(a),
      Exit::Failure(c) => on_failure(policy, make, used, acc, c),
    })
  })
}

fn pause<E: Send + 'static, R: Send + Sync + 'static>(d: Duration) -> Effect<(), E, R> {
  if d.is_zero() { yield_fiber() } else { sleep(d) }
}

fn on_failure<A, E, R>(
  policy: Arc<SupervisorPolicy<A>>,
  make: Make<A, E, R>,
  used: u64,
  acc: Option<Cause<E>>,
  cause: Cause<E>,
) -> Effect<A, E, R>
where
  A: Clone + Send + Sync + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  if ExitKind::of_cause(&cause) == ExitKind::Interrupt {
    return fail_cause(cause);
  }
  match &*policy {
    SupervisorPolicy::Terminate | SupervisorPolicy::Escalate => fail_cause(cause),
    SupervisorPolicy::Ignore { recover } => succeed(recover.clone()),
    SupervisorPolicy::Restart { delay } => {
      let d = *delay;
      pause(d).then(attempt(policy, make, used, acc))
    }
    SupervisorPolicy::RestartWithLimit { limit, delay } => {
      let all = match acc {
        None => cause,
        Some(prev) => Cause::then(prev, cause),
      };
      if used >= *limit {
        return fail_cause(all);
      }
      let d = *delay;
      pause(d).then(attempt(policy, make, used + 1, Some(all)))
    }
  }
}

/// Owns supervised fibers: each one is forked into the supervisor's [`Scope`], and
/// [`shutdown`](Self::shutdown) (or closing a parent it is [attached](Self::attach_to) to)
/// interrupts and awaits them all. Cloning yields another handle to the same supervisor.
#[derive(Clone, Debug, Default)]
pub struct Supervisor {
  scope: Scope,
}

impl Supervisor {
  /// A new, running supervisor.
  pub fn new() -> Self {
    Self {
      scope: Scope::new(),
    }
  }

  /// The supervisor's scope (for resources that should live exactly as long as it).
  pub fn scope(&self) -> &Scope {
    &self.scope
  }

  /// `true` once the supervisor was shut down.
  pub fn is_shutdown(&self) -> bool {
    self.scope.is_closed()
  }

  /// Forks `eff` under this supervisor: the fiber outlives the forking fiber and is interrupted
  /// when the supervisor shuts down. On a shut-down supervisor the fiber is interrupted before it
  /// runs.
  pub fn fork<A, E, R>(&self, eff: Effect<A, E, R>) -> Effect<FiberHandle<A, E>, Never, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.scope.fork_in(eff)
  }

  /// Forks the restart loop [`supervised`]`(policy, make)` under this supervisor.
  pub fn spawn<A, E, R, F>(
    &self,
    policy: SupervisorPolicy<A>,
    make: F,
  ) -> Effect<FiberHandle<A, E>, Never, R>
  where
    A: Clone + Send + Sync + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
    F: FnMut() -> Effect<A, E, R> + Send + 'static,
  {
    self.fork(supervised(policy, make))
  }

  /// Shuts the supervisor down: interrupts and awaits every fiber forked into it and runs its
  /// scope's finalizers. Idempotent.
  pub fn shutdown<E, R>(&self) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    hnil_in(self.scope.close(ExitKind::Interrupt).widen())
  }

  /// Ties this supervisor to `parent`: closing `parent` shuts the supervisor down.
  pub fn attach_to<E, R>(&self, parent: &Scope) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    parent.add_finalizer(self.scope.close(ExitKind::Interrupt))
  }
}
