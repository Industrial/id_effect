//! Decision events: every authorization decision as a [`DecisionEvent`] delivered to the
//! [`AuthzEvents`] sink capability.
//!
//! Decisions are recorded where they are made: by a [`Policies`](crate::Policies) registry built
//! with [`PoliciesBuilder::events`](crate::PoliciesBuilder::events) (every named evaluation, so
//! [`authorize`](crate::authorize) and [`check`](crate::check) too), by a policy wrapped with
//! [`AuthzEvents::observe`], and by a route guard (`Guard::events`, feature `axum`).
//!
//! A sink is anything implementing [`AuthzSink`]: [`MemorySink`] keeps events in memory for
//! tests, [`AuthzEvents::from_fn`] subscribes a closure (a logger, a metrics counter, a channel),
//! [`AuthzEvents::and`] fans out to several. Inside an effect, [`emit_decision`] reads the sink
//! from the environment. The event type is stable and serde-serialisable so an audit log can
//! persist it.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_authz::{
//!   AuthzEvents, BasicPrincipal, CurrentPrincipal, MemorySink, Outcome, Policies, require_role,
//! };
//!
//! let sink = MemorySink::new();
//! let policies = Policies::builder()
//!   .define::<()>("reports.export", require_role("auditor"))
//!   .events(AuthzEvents::new(sink.clone()))
//!   .build();
//!
//! let ada = CurrentPrincipal::new(BasicPrincipal::new("ada").with_role("auditor"));
//! let _ = policies.evaluate("reports.export", &ada, &()).block_on();
//! let sam = CurrentPrincipal::new(BasicPrincipal::new("sam"));
//! let _ = policies.evaluate("reports.export", &sam, &()).block_on();
//!
//! let events = sink.events();
//! assert_eq!(events[0].principal, "ada");
//! assert_eq!(events[0].outcome, Outcome::Allow);
//! assert_eq!(events[1].outcome, Outcome::Deny);
//! assert_eq!(events[1].reason.as_deref(), Some("requires role auditor"));
//! ```

use std::any::type_name;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, fail, need, succeed, sync};
use serde::{Deserialize, Serialize};

use crate::policy::{AuthzError, Decision, Policy, PolicyEffect};
use crate::principal::CurrentPrincipal;

/// What an authorization decision concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
  /// The policy allowed.
  Allow,
  /// The policy denied.
  Deny,
  /// The policy could not be evaluated (unknown policy, resource mismatch, failed lookup).
  Error,
}

/// One authorization decision: who, which policy, on what, and the outcome with its reason.
///
/// The serialised shape (field names, `outcome` as `"allow" | "deny" | "error"`) is stable:
/// audit logs persist it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DecisionEvent {
  /// The principal's id (`""` for the anonymous principal).
  pub principal: String,
  /// Whether the principal was authenticated.
  pub authenticated: bool,
  /// The principal's tenant, if any.
  pub tenant: Option<String>,
  /// The policy evaluated (its registry name, guard name, or observed name).
  pub policy: String,
  /// What was accessed (a resource type name, or `METHOD /path` for a route guard).
  pub resource: String,
  /// The outcome.
  pub outcome: Outcome,
  /// The denial reason or the evaluation error; `None` when allowed.
  pub reason: Option<String>,
}

impl DecisionEvent {
  /// The event for `principal` evaluating `policy` on `resource` with `result`.
  pub fn new(
    principal: &CurrentPrincipal,
    policy: impl Into<String>,
    resource: impl Into<String>,
    result: Result<&Decision, &AuthzError>,
  ) -> Self {
    let (outcome, reason) = match result {
      Ok(Decision::Allow) => (Outcome::Allow, None),
      Ok(Decision::Deny(denial)) => (Outcome::Deny, Some(denial.reason().to_owned())),
      Err(error) => (Outcome::Error, Some(error.to_string())),
    };
    Self {
      principal: principal.id().to_owned(),
      authenticated: principal.is_authenticated(),
      tenant: principal.tenant().map(str::to_owned),
      policy: policy.into(),
      resource: resource.into(),
      outcome,
      reason,
    }
  }

  /// Whether the decision allowed.
  pub fn is_allowed(&self) -> bool {
    self.outcome == Outcome::Allow
  }
}

impl fmt::Display for DecisionEvent {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let who = if self.authenticated {
      self.principal.as_str()
    } else {
      "<anonymous>"
    };
    write!(
      f,
      "{who} {} {} on {}",
      match self.outcome {
        Outcome::Allow => "allowed by",
        Outcome::Deny => "denied by",
        Outcome::Error => "failed evaluating",
      },
      self.policy,
      self.resource
    )?;
    match &self.reason {
      Some(reason) => write!(f, ": {reason}"),
      None => Ok(()),
    }
  }
}

/// Receives decision events. Implementations must be cheap and must not block: they run on the
/// request path.
pub trait AuthzSink: Send + Sync + 'static {
  /// Record `event`.
  fn record(&self, event: &DecisionEvent);
}

/// The decision-event capability: a shared handle to zero or more [`AuthzSink`]s. Cheap to clone.
#[derive(Clone, Default)]
pub struct AuthzEvents(Arc<[Arc<dyn AuthzSink>]>);

impl AuthzEvents {
  /// Events delivered to `sink`.
  pub fn new(sink: impl AuthzSink) -> Self {
    Self(Arc::new([Arc::new(sink) as Arc<dyn AuthzSink>]))
  }

  /// Events dropped (no sink).
  pub fn none() -> Self {
    Self::default()
  }

  /// Events delivered to the closure `subscriber`.
  pub fn from_fn(subscriber: impl Fn(&DecisionEvent) + Send + Sync + 'static) -> Self {
    Self::new(FnSink(subscriber))
  }

  /// Events delivered to the sinks of `self`, then to those of `other`.
  #[must_use]
  pub fn and(self, other: AuthzEvents) -> Self {
    Self(self.0.iter().chain(other.0.iter()).cloned().collect())
  }

  /// Number of sinks.
  pub fn sinks(&self) -> usize {
    self.0.len()
  }

  /// Deliver `event` to every sink, in order.
  pub fn record(&self, event: &DecisionEvent) {
    for sink in self.0.iter() {
      sink.record(event);
    }
  }

  /// An effect delivering `event` when it runs.
  pub fn emit<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
    event: DecisionEvent,
  ) -> Effect<(), E, R> {
    let events = self.clone();
    sync(move || events.record(&event))
  }

  /// `policy` recording each of its decisions as `name`; the resource is described by its type
  /// name.
  pub fn observe<P>(&self, name: impl Into<String>, policy: P) -> Observed<P> {
    Observed {
      name: name.into(),
      policy,
      events: self.clone(),
    }
  }
}

impl fmt::Debug for AuthzEvents {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AuthzEvents")
      .field("sinks", &self.sinks())
      .finish()
  }
}

struct FnSink<F>(F);

impl<F: Fn(&DecisionEvent) + Send + Sync + 'static> AuthzSink for FnSink<F> {
  fn record(&self, event: &DecisionEvent) {
    (self.0)(event);
  }
}

/// An in-memory [`AuthzSink`] for tests; clones share the same buffer.
#[derive(Clone, Debug, Default)]
pub struct MemorySink(Arc<Mutex<Vec<DecisionEvent>>>);

impl MemorySink {
  /// An empty sink.
  pub fn new() -> Self {
    Self::default()
  }

  /// Every recorded event, oldest first.
  pub fn events(&self) -> Vec<DecisionEvent> {
    self.lock().clone()
  }

  /// Removes and returns every recorded event.
  pub fn take(&self) -> Vec<DecisionEvent> {
    std::mem::take(&mut *self.lock())
  }

  /// Number of recorded events.
  pub fn len(&self) -> usize {
    self.lock().len()
  }

  /// Whether nothing was recorded.
  pub fn is_empty(&self) -> bool {
    self.lock().is_empty()
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Vec<DecisionEvent>> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl AuthzSink for MemorySink {
  fn record(&self, event: &DecisionEvent) {
    self.lock().push(event.clone());
  }
}

/// See [`AuthzEvents::observe`].
#[derive(Clone, Debug)]
pub struct Observed<P> {
  name: String,
  policy: P,
  events: AuthzEvents,
}

impl<Res: ?Sized, P: Policy<Res>> Policy<Res> for Observed<P> {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    audited(
      self.policy.evaluate(principal, resource),
      &self.events,
      principal,
      &self.name,
      type_name::<Res>(),
    )
  }
}

/// `effect` recording its decision (or typed failure) to `events`.
pub(crate) fn audited(
  effect: PolicyEffect,
  events: &AuthzEvents,
  principal: &CurrentPrincipal,
  policy: &str,
  resource: &str,
) -> PolicyEffect {
  let on_error = (
    events.clone(),
    principal.clone(),
    policy.to_owned(),
    resource.to_owned(),
  );
  let events = events.clone();
  let principal = principal.clone();
  let (policy, resource) = (policy.to_owned(), resource.to_owned());
  effect
    .flat_map(move |decision| {
      let event = DecisionEvent::new(&principal, policy, resource, Ok(&decision));
      events.emit(event).map(move |()| decision)
    })
    .catch(move |error| {
      let (events, principal, policy, resource) = on_error;
      let event = DecisionEvent::new(&principal, policy, resource, Err(&error));
      events.emit(event).flat_map(move |()| fail(error))
    })
}

/// The minimal typed environment holding only [`AuthzEvents`].
pub type AuthzEventsEnv = HCons<AuthzEvents, HNil>;

/// A [`Layer`] providing `events` as a capability.
#[inline]
pub fn provide_authz_events<E: Send + 'static>(events: AuthzEvents) -> Layer<AuthzEventsEnv, E> {
  Layer::succeed(id_effect::hlist![events])
}

/// Delivers `event` to the [`AuthzEvents`] in `R`.
pub fn emit_decision<E, R, I>(event: DecisionEvent) -> Effect<(), E, R>
where
  E: Send + 'static,
  R: Selector<AuthzEvents, I> + HList,
{
  need::<AuthzEvents>().flat_map(move |events| {
    events.record(&event);
    succeed(())
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::policy::{Denial, allow_all, deny_all, from_effect};
  use crate::principal::BasicPrincipal;
  use id_effect::{Cause, Exit};

  fn ada() -> CurrentPrincipal {
    CurrentPrincipal::new(BasicPrincipal::new("ada").with_tenant("acme"))
  }

  #[test]
  fn events_describe_the_decision_and_serialise() {
    let allowed = DecisionEvent::new(&ada(), "p", "Doc", Ok(&Decision::Allow));
    assert!(allowed.is_allowed());
    assert_eq!(allowed.tenant.as_deref(), Some("acme"));
    assert_eq!(allowed.to_string(), "ada allowed by p on Doc");
    let denied = DecisionEvent::new(
      &CurrentPrincipal::anonymous(),
      "p",
      "Doc",
      Ok(&Decision::Deny(Denial::new("no"))),
    );
    assert_eq!(denied.to_string(), "<anonymous> denied by p on Doc: no");
    let failed = DecisionEvent::new(&ada(), "p", "Doc", Err(&AuthzError::Evaluation("x".into())));
    assert_eq!(failed.outcome, Outcome::Error);
    assert_eq!(
      failed.to_string(),
      "ada failed evaluating p on Doc: policy evaluation failed: x"
    );

    let json = serde_json::to_value(&denied).expect("serialise");
    assert_eq!(json["outcome"], "deny");
    assert_eq!(json["principal"], "");
    assert_eq!(json["authenticated"], false);
    let back: DecisionEvent = serde_json::from_value(json).expect("deserialise");
    assert_eq!(back, denied);
  }

  #[test]
  fn sinks_fan_out_and_memory_sink_buffers() {
    let memory = MemorySink::new();
    let seen = Arc::new(Mutex::new(0_usize));
    let counter = Arc::clone(&seen);
    let events = AuthzEvents::new(memory.clone())
      .and(AuthzEvents::from_fn(move |_| *counter.lock().unwrap() += 1))
      .and(AuthzEvents::none());
    assert_eq!(events.sinks(), 2);
    assert_eq!(format!("{events:?}"), "AuthzEvents { sinks: 2 }");
    assert!(memory.is_empty());
    events.record(&DecisionEvent::new(&ada(), "p", "r", Ok(&Decision::Allow)));
    let emitted: Effect<(), (), HNil> =
      events.emit(DecisionEvent::new(&ada(), "q", "r", Ok(&Decision::Allow)));
    assert_eq!(emitted.block_on(), Exit::Success(()));
    assert_eq!(memory.len(), 2);
    assert_eq!(*seen.lock().unwrap(), 2);
    assert_eq!(memory.take().len(), 2);
    assert!(memory.events().is_empty());
  }

  #[test]
  fn observed_policies_record_decisions_and_failures() {
    let memory = MemorySink::new();
    let events = AuthzEvents::new(memory.clone());
    let yes = events.observe("yes", allow_all());
    let no = events.observe("no", deny_all("closed"));
    let broken = events.observe(
      "broken",
      from_effect(|_: &CurrentPrincipal, _: &u8| fail(AuthzError::Evaluation("db down".into()))),
    );
    assert_eq!(
      yes.evaluate(&ada(), &()).block_on(),
      Exit::Success(Decision::Allow)
    );
    assert!(matches!(
      no.evaluate(&ada(), &()).block_on(),
      Exit::Success(Decision::Deny(_))
    ));
    assert_eq!(
      broken.evaluate(&ada(), &7).block_on(),
      Exit::Failure(Cause::Fail(AuthzError::Evaluation("db down".into())))
    );
    let recorded = memory.events();
    let outcomes: Vec<_> = recorded
      .iter()
      .map(|e| (e.policy.as_str(), e.outcome))
      .collect();
    assert_eq!(
      outcomes,
      [
        ("yes", Outcome::Allow),
        ("no", Outcome::Deny),
        ("broken", Outcome::Error)
      ]
    );
    assert_eq!(recorded[2].resource, "u8");
    assert!(format!("{yes:?}").contains("yes"));
  }

  #[test]
  fn the_capability_is_read_from_the_environment() {
    let memory = MemorySink::new();
    type Env = HCons<AuthzEvents, HNil>;
    let event = DecisionEvent::new(&ada(), "p", "r", Ok(&Decision::Allow));
    let eff: Effect<(), (), Env> = emit_decision(event);
    let run = eff.provide_layer(provide_authz_events(AuthzEvents::new(memory.clone())));
    assert_eq!(run.block_on(), Exit::Success(()));
    assert_eq!(memory.events()[0].policy, "p");
  }
}
