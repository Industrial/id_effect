//! Attribute-based policies over a principal's claims ([`Principal::claim`](crate::Principal::claim)):
//! [`claim_equals`], [`claim_contains`], [`claim`] and, comparing a claim with the resource,
//! [`claim_matches`]. A missing claim always denies.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision, Policy, PolicyExt, claim, claim_equals};
//!
//! let finance_lead = claim_equals("department", "finance")
//!   .and(claim("level", |v| v.as_u64().is_some_and(|l| l >= 3)));
//!
//! let dana = CurrentPrincipal::new(
//!   BasicPrincipal::new("dana").with_claim("department", "finance").with_claim("level", 4),
//! );
//! assert_eq!(finance_lead.evaluate(&dana, &()).block_on(), Exit::Success(Decision::Allow));
//! ```

use std::fmt;

use id_effect::succeed;
use serde_json::Value;

use crate::policy::{Decision, Policy, PolicyEffect};
use crate::principal::CurrentPrincipal;

/// Allows when the claim `name` equals `value`.
pub fn claim_equals(name: impl Into<String>, value: impl Into<Value>) -> ClaimPolicy<ClaimEquals> {
  let name = name.into();
  ClaimPolicy {
    reason: format!("claim {name} does not match"),
    name,
    check: ClaimEquals(value.into()),
  }
}

/// Allows when the claim `name` is an array containing `value`, or equals it.
pub fn claim_contains(
  name: impl Into<String>,
  value: impl Into<Value>,
) -> ClaimPolicy<ClaimContains> {
  let name = name.into();
  ClaimPolicy {
    reason: format!("claim {name} does not contain the required value"),
    name,
    check: ClaimContains(value.into()),
  }
}

/// Allows when the claim `name` is present and satisfies `check`.
pub fn claim<F>(name: impl Into<String>, check: F) -> ClaimPolicy<F>
where
  F: Fn(&Value) -> bool + Send + Sync + 'static,
{
  let name = name.into();
  ClaimPolicy {
    reason: format!("claim {name} does not satisfy the policy"),
    name,
    check,
  }
}

/// A test of one claim value: [`ClaimEquals`], [`ClaimContains`] or any
/// `Fn(&Value) -> bool`.
pub trait ClaimCheck: Send + Sync + 'static {
  /// Whether `value` passes.
  fn check(&self, value: &Value) -> bool;
}

impl<F: Fn(&Value) -> bool + Send + Sync + 'static> ClaimCheck for F {
  fn check(&self, value: &Value) -> bool {
    self(value)
  }
}

/// See [`claim_equals`].
#[derive(Clone, Debug)]
pub struct ClaimEquals(Value);

impl ClaimCheck for ClaimEquals {
  fn check(&self, value: &Value) -> bool {
    *value == self.0
  }
}

/// See [`claim_contains`].
#[derive(Clone, Debug)]
pub struct ClaimContains(Value);

impl ClaimCheck for ClaimContains {
  fn check(&self, value: &Value) -> bool {
    match value {
      Value::Array(items) => items.contains(&self.0),
      other => *other == self.0,
    }
  }
}

/// See [`claim_equals`], [`claim_contains`] and [`claim`].
#[derive(Clone)]
pub struct ClaimPolicy<C> {
  name: String,
  reason: String,
  check: C,
}

impl<C> fmt::Debug for ClaimPolicy<C> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ClaimPolicy")
      .field("claim", &self.name)
      .finish()
  }
}

impl<Res: ?Sized, C: ClaimCheck> Policy<Res> for ClaimPolicy<C> {
  fn evaluate(&self, principal: &CurrentPrincipal, _: &Res) -> PolicyEffect {
    let passes = principal
      .claim(&self.name)
      .is_some_and(|v| self.check.check(v));
    succeed(Decision::allow_if(passes, self.reason.as_str()))
  }
}

/// Allows when the claim `name` is present and `check(claim, resource)` holds (e.g. the
/// principal's `region` claim equals the resource's region).
pub fn claim_matches<Res, F>(name: impl Into<String>, check: F) -> ClaimMatches<F>
where
  Res: ?Sized,
  F: Fn(&Value, &Res) -> bool + Send + Sync + 'static,
{
  let name = name.into();
  ClaimMatches {
    reason: format!("claim {name} does not match the resource"),
    name,
    check,
  }
}

/// See [`claim_matches`].
#[derive(Clone)]
pub struct ClaimMatches<F> {
  name: String,
  reason: String,
  check: F,
}

impl<F> fmt::Debug for ClaimMatches<F> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ClaimMatches")
      .field("claim", &self.name)
      .finish()
  }
}

impl<Res, F> Policy<Res> for ClaimMatches<F>
where
  Res: ?Sized,
  F: Fn(&Value, &Res) -> bool + Send + Sync + 'static,
{
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    let passes = principal
      .claim(&self.name)
      .is_some_and(|v| (self.check)(v, resource));
    succeed(Decision::allow_if(passes, self.reason.as_str()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::principal::BasicPrincipal;
  use id_effect::Exit;
  use serde_json::json;

  fn decide<Res, P: Policy<Res>>(policy: &P, who: &BasicPrincipal, res: &Res) -> Decision {
    match policy
      .evaluate(&CurrentPrincipal::new(who.clone()), res)
      .block_on()
    {
      Exit::Success(d) => d,
      other => panic!("unexpected exit: {other:?}"),
    }
  }

  fn dana() -> BasicPrincipal {
    BasicPrincipal::new("dana")
      .with_claim("department", "finance")
      .with_claim("groups", json!(["approvers", "staff"]))
      .with_claim("region", "eu")
      .with_claim("level", 2)
  }

  #[test]
  fn equals_and_missing_claims() {
    assert!(decide(&claim_equals("department", "finance"), &dana(), &()).is_allowed());
    assert_eq!(
      decide(&claim_equals("department", "sales"), &dana(), &()),
      Decision::deny("claim department does not match")
    );
    assert!(decide(&claim_equals("absent", "x"), &dana(), &()).is_denied());
    assert!(format!("{:?}", claim_equals("department", "x")).contains("department"));
  }

  #[test]
  fn contains_handles_arrays_and_scalars() {
    assert!(decide(&claim_contains("groups", "approvers"), &dana(), &()).is_allowed());
    assert_eq!(
      decide(&claim_contains("groups", "admins"), &dana(), &()),
      Decision::deny("claim groups does not contain the required value")
    );
    assert!(decide(&claim_contains("region", "eu"), &dana(), &()).is_allowed());
  }

  #[test]
  fn predicate_over_a_claim() {
    let senior = claim("level", |v: &Value| v.as_u64().is_some_and(|l| l >= 3));
    assert_eq!(
      decide(&senior, &dana(), &()),
      Decision::deny("claim level does not satisfy the policy")
    );
    let junior = claim("level", |v: &Value| v.as_u64() == Some(2));
    assert!(decide(&junior, &dana(), &()).is_allowed());
  }

  #[test]
  fn claim_compared_with_the_resource() {
    struct Doc {
      region: &'static str,
    }
    let same_region = claim_matches("region", |v: &Value, d: &Doc| v == d.region);
    assert!(decide(&same_region, &dana(), &Doc { region: "eu" }).is_allowed());
    assert_eq!(
      decide(&same_region, &dana(), &Doc { region: "us" }),
      Decision::deny("claim region does not match the resource")
    );
    assert!(format!("{same_region:?}").contains("region"));
  }
}
