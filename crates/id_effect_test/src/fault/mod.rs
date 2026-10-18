//! Fault injection (feature `fault`): latency, failures and hangs injected into any [`Effect`] —
//! the calls of a service double (`TestSqlClient`, the HTTP `TestClient`) or of a real client —
//! on a seeded, reproducible schedule.
//!
//! - **[`Fault`]** — one fault: [`Fault::delay`] (the call starts `d` later),
//!   [`Fault::fail`] (the call fails with `e` and does not run), [`Fault::timeout`] (the call
//!   never answers: only the caller's own deadline, such as [`Effect::timeout`], ends it).
//!   [`Fault::apply`] injects it into one call.
//! - **[`FaultPlan`]** — which call gets which fault. Each call injected with
//!   [`FaultPlan::inject`] draws once, when it starts, from a generator seeded by
//!   [`FaultPlan::seeded`], against the plan's rules (`with(probability, fault)`): the same seed
//!   and the same calls give the same schedule, recorded in [`FaultPlan::history`].
//!
//! Delays are [`sleep`]s on the fiber's executor clock: on a
//! [`TestExecutor`](id_effect::TestExecutor) they are virtual time, moved by `advance`, so a test
//! with 30 s of injected latency takes no wall time.
//!
//! ```rust
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! use id_effect::{Effect, TestExecutor, succeed};
//! use id_effect_test::fault::{Fault, FaultKind, FaultPlan};
//!
//! let plan = FaultPlan::seeded(42)
//!   .with(0.5, Fault::delay(Duration::from_secs(30)))
//!   .with(0.25, Fault::fail("injected"));
//! let call = || -> Effect<u32, &'static str> { succeed(1) };
//!
//! let exec = TestExecutor::new();
//! for _ in 0..8 {
//!   let h = plan.inject(call()).fork_root(Arc::new(exec.clone()));
//!   exec.advance(Duration::from_secs(30)); // virtual: returns at once
//!   assert!(h.is_done());
//! }
//! assert_eq!(plan.calls(), 8);
//!
//! // The same seed decides the same faults for the same calls.
//! let again = FaultPlan::seeded(42)
//!   .with(0.5, Fault::delay(Duration::from_secs(30)))
//!   .with(0.25, Fault::fail("injected"));
//! let schedule: Vec<Option<FaultKind>> =
//!   (0..8).map(|_| again.next_fault().map(|f| f.kind())).collect();
//! assert_eq!(schedule, plan.history());
//! ```

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::{Effect, Random, fail, from_async, sleep, suspend};

#[cfg(test)]
mod tests;

/// One injected fault, for calls failing with `E`. Build it with [`delay`](Self::delay),
/// [`fail`](Self::fail) or [`timeout`](Self::timeout); inject it with [`apply`](Self::apply) or
/// through a [`FaultPlan`].
#[derive(Clone, PartialEq, Eq)]
pub struct Fault<E>(Kind<E>);

#[derive(Clone, PartialEq, Eq)]
enum Kind<E> {
  Delay(Duration),
  Fail(E),
  Timeout,
}

/// What a [`Fault`] does, without its error value: an entry of [`FaultPlan::history`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaultKind {
  /// The call started this much later.
  Delay(Duration),
  /// The call failed without running.
  Fail,
  /// The call never answered.
  Timeout,
}

impl<E> Fault<E> {
  /// Latency: the call runs after a [`sleep`] of `d` on the executor clock (virtual time on a
  /// `TestExecutor`), then answers as it would have.
  pub fn delay(d: Duration) -> Self {
    Self(Kind::Delay(d))
  }

  /// A failure: the call fails with `error` and does not run.
  pub fn fail(error: E) -> Self {
    Self(Kind::Fail(error))
  }

  /// A hang: the call never answers and does not run, as a dependency that accepted the request
  /// and went silent. Only the caller's own deadline ([`Effect::timeout`], `timeout_fail`) or an
  /// interrupt ends it; without one, the caller waits forever.
  pub fn timeout() -> Self {
    Self(Kind::Timeout)
  }

  /// What this fault does.
  pub fn kind(&self) -> FaultKind {
    match &self.0 {
      Kind::Delay(d) => FaultKind::Delay(*d),
      Kind::Fail(_) => FaultKind::Fail,
      Kind::Timeout => FaultKind::Timeout,
    }
  }
}

impl<E: Clone + Send + 'static> Fault<E> {
  /// Injects this fault into `call` (a clone of the error for [`Fault::fail`], so one fault can be
  /// applied to many calls).
  pub fn apply<A, R>(&self, call: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    R: Send + Sync + 'static,
  {
    match &self.0 {
      Kind::Delay(d) => sleep(*d).then(call),
      Kind::Fail(e) => fail(e.clone()),
      Kind::Timeout => from_async(std::future::pending::<Result<A, E>>),
    }
  }
}

impl<E> fmt::Debug for Fault<E> {
  /// The fault's [`kind`](Self::kind) (`E` need not be `Debug`).
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("Fault").field(&self.kind()).finish()
  }
}

/// A seeded, reproducible fault schedule: which calls get which [`Fault`].
///
/// Each call through [`inject`](Self::inject) (or each [`next_fault`](Self::next_fault)) draws
/// one uniform number `u` in `[0, 1)` from the plan's generator and walks the rules in the order
/// they were added: the first rule whose cumulative probability exceeds `u` is the call's fault,
/// and a `u` past them all is no fault. One draw per call whatever the outcome, so the schedule
/// depends only on the seed, the rules and the number of calls before. Calls decide in the order
/// they start; run them sequentially (or on a single-threaded
/// [`TestExecutor`](id_effect::TestExecutor)) for that order to be reproducible.
///
/// Cloning gives another handle to the same plan (clones share the generator and the history),
/// so one plan can be moved into a retry factory or a service double and inspected afterwards.
pub struct FaultPlan<E> {
  inner: Arc<Mutex<Plan<E>>>,
}

struct Plan<E> {
  random: Random,
  seed: Option<u64>,
  rules: Vec<(f64, Fault<E>)>,
  history: Vec<Option<FaultKind>>,
}

impl<E> FaultPlan<E> {
  /// A plan drawing from a generator seeded by `seed`, with no rules yet (no call is faulted
  /// until [`with`](Self::with) adds one).
  pub fn seeded(seed: u64) -> Self {
    Self::new(Random::seeded(seed), Some(seed))
  }

  /// A plan drawing from `random` — a `TestRandom` (`TestRandom::random()`) whose queued
  /// `f64`s pick each call's fault exactly.
  pub fn from_random(random: Random) -> Self {
    Self::new(random, None)
  }

  fn new(random: Random, seed: Option<u64>) -> Self {
    Self {
      inner: Arc::new(Mutex::new(Plan {
        random,
        seed,
        rules: Vec::new(),
        history: Vec::new(),
      })),
    }
  }

  fn state(&self) -> MutexGuard<'_, Plan<E>> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Adds a rule: each call gets `fault` with `probability` (in `[0, 1]`). The rules are
  /// exclusive — a call gets at most one fault — so their probabilities add up.
  ///
  /// # Panics
  ///
  /// When `probability` is not in `[0, 1]`, or the rules' probabilities add up to more than 1.
  #[must_use]
  #[track_caller]
  pub fn with(self, probability: f64, fault: Fault<E>) -> Self {
    assert!(
      (0.0..=1.0).contains(&probability),
      "FaultPlan::with: probability {probability} is not in [0, 1]"
    );
    {
      let mut state = self.state();
      let total = state.rules.iter().map(|(p, _)| p).sum::<f64>() + probability;
      assert!(
        total <= 1.0 + 1e-9,
        "FaultPlan::with: the rules' probabilities add up to {total}, more than 1"
      );
      state.rules.push((probability, fault));
    }
    self
  }

  /// The seed of a [`seeded`](Self::seeded) plan (to report with a failing schedule).
  pub fn seed(&self) -> Option<u64> {
    self.state().seed
  }

  /// The number of calls decided so far.
  pub fn calls(&self) -> usize {
    self.state().history.len()
  }

  /// Each decided call's fault, in the order the calls started (`None`: not faulted).
  pub fn history(&self) -> Vec<Option<FaultKind>> {
    self.state().history.clone()
  }
}

impl<E: Clone> FaultPlan<E> {
  /// Decides the next call: one draw against the rules, recorded in the
  /// [`history`](Self::history). [`inject`](Self::inject) calls it when the call starts.
  pub fn next_fault(&self) -> Option<Fault<E>> {
    let mut state = self.state();
    let u = state.random.next_f64();
    let mut cumulative = 0.0;
    let fault = state
      .rules
      .iter()
      .find(|(p, _)| {
        cumulative += p;
        u < cumulative
      })
      .map(|(_, fault)| fault.clone());
    state.history.push(fault.as_ref().map(Fault::kind));
    fault
  }
}

impl<E: Clone + Send + 'static> FaultPlan<E> {
  /// Injects the plan into `call`: when the call starts, the plan decides its fault
  /// ([`next_fault`](Self::next_fault)) and [`applies`](Fault::apply) it, or runs the call as is.
  /// Wrap each attempt (inside a retry's factory) for each attempt to draw.
  pub fn inject<A, R>(&self, call: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    R: Send + Sync + 'static,
  {
    let plan = self.clone();
    suspend(move || match plan.next_fault() {
      Some(fault) => fault.apply(call),
      None => call,
    })
  }
}

impl<E> Clone for FaultPlan<E> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<E> fmt::Debug for FaultPlan<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let state = self.state();
    f.debug_struct("FaultPlan")
      .field("seed", &state.seed)
      .field(
        "rules",
        &state
          .rules
          .iter()
          .map(|(p, fault)| (*p, fault.kind()))
          .collect::<Vec<_>>(),
      )
      .field("calls", &state.history.len())
      .finish()
  }
}
