//! Test helpers: [`AuthzTest`] asserts what a policy decides and keeps every decision it saw.
//!
//! Enabled with the `testing` feature (add `id_effect_authz = { .., features = ["testing"] }`
//! to `[dev-dependencies]`).
//!
//! ```
//! use id_effect_authz::testing::AuthzTest;
//! use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Outcome, Policies, PolicyExt, Rbac};
//!
//! let rbac = Rbac::builder().grant("accountant", ["ledger:post"]).build();
//! let authz = AuthzTest::with_policies(
//!   Policies::builder().define::<()>("ledger.post", rbac.require("ledger:post")).build(),
//! );
//!
//! let carl = CurrentPrincipal::new(BasicPrincipal::new("carl").with_role("accountant"));
//! let vera = CurrentPrincipal::new(BasicPrincipal::new("vera").with_role("viewer"));
//! authz.assert_allows_named(&carl, "ledger.post", &());
//! authz.assert_denied_because_named(&vera, "ledger.post", &(), "missing permission ledger:post");
//!
//! // The direct form takes any policy.
//! authz.assert_allows(&carl, &rbac.require("ledger:post"), &());
//!
//! let events = authz.events();
//! assert_eq!(events.len(), 3);
//! assert_eq!(events[1].principal, "vera");
//! assert_eq!(events[1].outcome, Outcome::Deny);
//! ```

use std::any::type_name;

use id_effect::Exit;

use crate::audit::{AuthzEvents, DecisionEvent, MemorySink, audited};
use crate::policy::{Decision, Denial, Policy, PolicyEffect};
use crate::principal::CurrentPrincipal;
use crate::registry::Policies;

/// Runs policies for tests, asserts their decisions, and records every decision in a
/// [`MemorySink`].
///
/// Plug [`AuthzTest::sink`] into a route guard (`Guard::events`) to observe the decisions an
/// HTTP test triggers too.
#[derive(Clone, Debug, Default)]
pub struct AuthzTest {
  policies: Policies,
  sink: MemorySink,
}

impl AuthzTest {
  /// A harness without named policies (use the direct assertions).
  pub fn new() -> Self {
    Self::default()
  }

  /// A harness over `policies`; their evaluations are recorded (besides any sink they already
  /// had).
  pub fn with_policies(policies: Policies) -> Self {
    let sink = MemorySink::new();
    let mine = AuthzEvents::new(sink.clone());
    let events = match policies.events() {
      Some(existing) => existing.clone().and(mine),
      None => mine,
    };
    Self {
      policies: policies.with_events(events),
      sink,
    }
  }

  /// The policies, recording into this harness (provide them to the code under test).
  pub fn policies(&self) -> &Policies {
    &self.policies
  }

  /// The sink this harness records into, as a capability.
  pub fn sink(&self) -> AuthzEvents {
    AuthzEvents::new(self.sink.clone())
  }

  /// Every decision recorded so far, oldest first.
  pub fn events(&self) -> Vec<DecisionEvent> {
    self.sink.events()
  }

  /// Removes and returns every decision recorded so far.
  pub fn take_events(&self) -> Vec<DecisionEvent> {
    self.sink.take()
  }

  /// What `policy` decides for `principal` on `resource` (recorded under the policy's type name).
  ///
  /// # Panics
  ///
  /// When the policy fails to evaluate.
  #[track_caller]
  pub fn decide<Res, P>(&self, principal: &CurrentPrincipal, policy: &P, resource: &Res) -> Decision
  where
    Res: ?Sized,
    P: Policy<Res> + ?Sized,
  {
    let name = type_name::<P>();
    let effect = audited(
      policy.evaluate(principal, resource),
      &self.sink(),
      principal,
      name,
      type_name::<Res>(),
    );
    run(effect, principal, name)
  }

  /// What the named policy decides for `principal` on `resource`.
  ///
  /// # Panics
  ///
  /// When the policy is unknown, defined for another resource type, or fails to evaluate.
  #[track_caller]
  pub fn decide_named<Res: ?Sized + 'static>(
    &self,
    principal: &CurrentPrincipal,
    name: &str,
    resource: &Res,
  ) -> Decision {
    run(
      self.policies.evaluate(name, principal, resource),
      principal,
      name,
    )
  }

  /// Asserts that `policy` allows `principal` on `resource`.
  ///
  /// # Panics
  ///
  /// When the policy denies or fails.
  #[track_caller]
  pub fn assert_allows<Res, P>(&self, principal: &CurrentPrincipal, policy: &P, resource: &Res)
  where
    Res: ?Sized,
    P: Policy<Res> + ?Sized,
  {
    expect_allow(
      self.decide(principal, policy, resource),
      principal,
      type_name::<P>(),
    );
  }

  /// Asserts that `policy` denies `principal` on `resource`; returns the denial.
  ///
  /// # Panics
  ///
  /// When the policy allows or fails.
  #[track_caller]
  pub fn assert_denies<Res, P>(
    &self,
    principal: &CurrentPrincipal,
    policy: &P,
    resource: &Res,
  ) -> Denial
  where
    Res: ?Sized,
    P: Policy<Res> + ?Sized,
  {
    expect_deny(
      self.decide(principal, policy, resource),
      principal,
      type_name::<P>(),
    )
  }

  /// Asserts that `policy` denies `principal` on `resource` with exactly `reason`.
  ///
  /// # Panics
  ///
  /// When the policy allows, fails, or denies for another reason.
  #[track_caller]
  pub fn assert_denied_because<Res, P>(
    &self,
    principal: &CurrentPrincipal,
    policy: &P,
    resource: &Res,
    reason: &str,
  ) where
    Res: ?Sized,
    P: Policy<Res> + ?Sized,
  {
    expect_reason(&self.assert_denies(principal, policy, resource), reason);
  }

  /// Asserts that the named policy allows `principal` on `resource`.
  ///
  /// # Panics
  ///
  /// When the policy denies or cannot be evaluated.
  #[track_caller]
  pub fn assert_allows_named<Res: ?Sized + 'static>(
    &self,
    principal: &CurrentPrincipal,
    name: &str,
    resource: &Res,
  ) {
    expect_allow(
      self.decide_named(principal, name, resource),
      principal,
      name,
    );
  }

  /// Asserts that the named policy denies `principal` on `resource`; returns the denial.
  ///
  /// # Panics
  ///
  /// When the policy allows or cannot be evaluated.
  #[track_caller]
  pub fn assert_denies_named<Res: ?Sized + 'static>(
    &self,
    principal: &CurrentPrincipal,
    name: &str,
    resource: &Res,
  ) -> Denial {
    expect_deny(
      self.decide_named(principal, name, resource),
      principal,
      name,
    )
  }

  /// Asserts that the named policy denies `principal` on `resource` with exactly `reason`.
  ///
  /// # Panics
  ///
  /// When the policy allows, cannot be evaluated, or denies for another reason.
  #[track_caller]
  pub fn assert_denied_because_named<Res: ?Sized + 'static>(
    &self,
    principal: &CurrentPrincipal,
    name: &str,
    resource: &Res,
    reason: &str,
  ) {
    expect_reason(&self.assert_denies_named(principal, name, resource), reason);
  }
}

#[track_caller]
fn run(effect: PolicyEffect, principal: &CurrentPrincipal, policy: &str) -> Decision {
  match effect.block_on() {
    Exit::Success(decision) => decision,
    Exit::Failure(cause) => panic!(
      "policy `{policy}` failed for principal `{}`: {cause:?}",
      principal.id()
    ),
  }
}

#[track_caller]
fn expect_allow(decision: Decision, principal: &CurrentPrincipal, policy: &str) {
  if let Decision::Deny(denial) = decision {
    panic!(
      "expected policy `{policy}` to allow principal `{}`, it denied: {}",
      principal.id(),
      denial.reason()
    );
  }
}

#[track_caller]
fn expect_deny(decision: Decision, principal: &CurrentPrincipal, policy: &str) -> Denial {
  match decision {
    Decision::Deny(denial) => denial,
    Decision::Allow => panic!(
      "expected policy `{policy}` to deny principal `{}`, it allowed",
      principal.id()
    ),
  }
}

#[track_caller]
fn expect_reason(denial: &Denial, reason: &str) {
  assert_eq!(
    denial.reason(),
    reason,
    "the policy denied for another reason"
  );
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::audit::{MemorySink, Outcome};
  use crate::policy::{allow_all, deny_all, require_role};
  use crate::principal::BasicPrincipal;

  fn carl() -> CurrentPrincipal {
    CurrentPrincipal::new(BasicPrincipal::new("carl").with_role("accountant"))
  }

  fn harness() -> AuthzTest {
    AuthzTest::with_policies(
      Policies::builder()
        .define::<()>("post", require_role("accountant"))
        .define::<()>("close", deny_all("owners only"))
        .build(),
    )
  }

  #[test]
  fn direct_assertions_record_events() {
    let t = AuthzTest::new();
    t.assert_allows(&carl(), &require_role("accountant"), &());
    let denial = t.assert_denies(&carl(), &deny_all("nope"), &());
    assert_eq!(denial.reason(), "nope");
    t.assert_denied_because(&carl(), &require_role("owner"), &(), "requires role owner");
    assert_eq!(t.decide(&carl(), &allow_all(), &1_u8), Decision::Allow);
    let events = t.take_events();
    assert_eq!(events.len(), 4);
    assert_eq!(events[3].resource, "u8");
    assert!(
      events[0].policy.contains("RequireRole"),
      "{}",
      events[0].policy
    );
    assert!(t.events().is_empty());
  }

  #[test]
  fn named_assertions_use_the_recording_registry() {
    let t = harness();
    t.assert_allows_named(&carl(), "post", &());
    let denial = t.assert_denies_named(&carl(), "close", &());
    assert_eq!(denial.policy(), Some("close"));
    t.assert_denied_because_named(&carl(), "close", &(), "owners only");
    assert!(t.policies().contains("post"));
    assert_eq!(t.sink().sinks(), 1);
    let outcomes: Vec<_> = t.events().iter().map(|e| e.outcome).collect();
    assert_eq!(outcomes, [Outcome::Allow, Outcome::Deny, Outcome::Deny]);
  }

  #[test]
  fn an_existing_sink_keeps_receiving() {
    let theirs = MemorySink::new();
    let t = AuthzTest::with_policies(
      Policies::builder()
        .define::<()>("open", allow_all())
        .events(AuthzEvents::new(theirs.clone()))
        .build(),
    );
    t.assert_allows_named(&carl(), "open", &());
    assert_eq!((theirs.len(), t.events().len()), (1, 1));
  }

  #[test]
  #[should_panic(expected = "to allow principal `carl`, it denied: owners only")]
  fn allow_assertion_panics_on_denial() {
    harness().assert_allows_named(&carl(), "close", &());
  }

  #[test]
  #[should_panic(expected = "to deny principal `carl`, it allowed")]
  fn deny_assertion_panics_on_allow() {
    harness().assert_denies_named(&carl(), "post", &());
  }

  #[test]
  #[should_panic(expected = "another reason")]
  fn reason_assertion_panics_on_mismatch() {
    harness().assert_denied_because_named(&carl(), "close", &(), "closed");
  }

  #[test]
  #[should_panic(expected = "policy `missing` failed for principal `carl`")]
  fn evaluation_failures_panic() {
    harness().assert_allows_named(&carl(), "missing", &());
  }
}
