//! Policies: a [`Policy<Res>`](Policy) evaluates a [`CurrentPrincipal`] against a resource `Res`
//! as an effect returning a [`Decision`].
//!
//! Policies compose with [`PolicyExt::and`], [`PolicyExt::or`], [`PolicyExt::negate`],
//! [`all_of`] and [`any_of`]. Simple predicates come from [`predicate`]; a policy that has to
//! look something up (an ACL row, a feature flag) is an effect, built with [`from_effect`].
//!
//! A policy that needs capabilities (a SQL client, a cache) receives them when it is
//! constructed, typically in a [`Layer`](id_effect::Layer), so evaluating it needs nothing more:
//! its effect has the empty environment.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_authz::{
//!   BasicPrincipal, CurrentPrincipal, Decision, Policy, PolicyExt, predicate, require_role,
//! };
//!
//! struct Invoice {
//!   amount: u64,
//! }
//!
//! let approve = require_role("accountant")
//!   .and(predicate("amount above the approval limit", |_, inv: &Invoice| inv.amount <= 10_000));
//!
//! let alice = CurrentPrincipal::new(BasicPrincipal::new("alice").with_role("accountant"));
//! let small = approve.evaluate(&alice, &Invoice { amount: 500 });
//! assert_eq!(small.block_on(), Exit::Success(Decision::Allow));
//! let large = approve.evaluate(&alice, &Invoice { amount: 50_000 });
//! assert!(matches!(large.block_on(), Exit::Success(Decision::Deny(_))));
//! ```

use std::fmt;
use std::sync::Arc;

use id_effect::observability::posture::record_dangerous;
use id_effect::{Effect, HList, Selector, need, succeed};

use crate::principal::CurrentPrincipal;
use crate::resource::{TenantScope, scoped};

/// Why a policy denied: a human-readable reason and, when evaluated by name through
/// [`Policies`](crate::Policies), the policy's name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Denial {
  reason: String,
  policy: Option<String>,
}

impl Denial {
  /// A denial with `reason`.
  pub fn new(reason: impl Into<String>) -> Self {
    Self {
      reason: reason.into(),
      policy: None,
    }
  }

  /// The reason.
  pub fn reason(&self) -> &str {
    &self.reason
  }

  /// The named policy that denied, when known.
  pub fn policy(&self) -> Option<&str> {
    self.policy.as_deref()
  }

  /// This denial attributed to the policy `name` (kept when already attributed).
  #[must_use]
  pub fn attributed_to(mut self, name: &str) -> Self {
    if self.policy.is_none() {
      self.policy = Some(name.to_owned());
    }
    self
  }
}

impl fmt::Display for Denial {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &self.policy {
      Some(name) => write!(f, "{name}: {}", self.reason),
      None => f.write_str(&self.reason),
    }
  }
}

/// The outcome of evaluating a policy.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Decision {
  /// Access is granted.
  Allow,
  /// Access is refused, with the reason.
  Deny(Denial),
}

impl Decision {
  /// A denial with `reason`.
  pub fn deny(reason: impl Into<String>) -> Self {
    Self::Deny(Denial::new(reason))
  }

  /// [`Allow`](Decision::Allow) when `allowed`, else a denial with `reason`.
  pub fn allow_if(allowed: bool, reason: impl Into<String>) -> Self {
    if allowed {
      Self::Allow
    } else {
      Self::deny(reason)
    }
  }

  /// Whether access is granted.
  pub fn is_allowed(&self) -> bool {
    matches!(self, Self::Allow)
  }

  /// Whether access is refused.
  pub fn is_denied(&self) -> bool {
    !self.is_allowed()
  }

  /// `Ok(())` when allowed; [`AuthzError::Unauthenticated`] when denied to an anonymous
  /// `principal`, [`AuthzError::Denied`] otherwise.
  pub fn into_result(self, principal: &CurrentPrincipal) -> Result<(), AuthzError> {
    match self {
      Self::Allow => Ok(()),
      Self::Deny(_) if !principal.is_authenticated() => Err(AuthzError::Unauthenticated),
      Self::Deny(denial) => Err(AuthzError::Denied(denial)),
    }
  }
}

/// Authorization failures.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AuthzError {
  /// The principal is anonymous and the policy denied: authenticate first (HTTP 401).
  Unauthenticated,
  /// The policy denied an authenticated principal (HTTP 403).
  Denied(Denial),
  /// No policy is registered under this name.
  UnknownPolicy(String),
  /// The named policy exists for another resource type.
  ResourceMismatch {
    /// The policy name.
    policy: String,
    /// The resource type the policy was registered for.
    expected: &'static str,
    /// The resource type it was evaluated with.
    found: &'static str,
  },
  /// Evaluating the policy failed (a lookup it depends on failed).
  Evaluation(String),
}

impl AuthzError {
  /// The HTTP status a route guard answers with: 401, 403 or 500.
  pub fn http_status(&self) -> u16 {
    match self {
      Self::Unauthenticated => 401,
      Self::Denied(_) => 403,
      Self::UnknownPolicy(_) | Self::ResourceMismatch { .. } | Self::Evaluation(_) => 500,
    }
  }
}

impl fmt::Display for AuthzError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Unauthenticated => f.write_str("authentication required"),
      Self::Denied(denial) => write!(f, "forbidden: {denial}"),
      Self::UnknownPolicy(name) => write!(f, "unknown policy `{name}`"),
      Self::ResourceMismatch {
        policy,
        expected,
        found,
      } => write!(
        f,
        "policy `{policy}` is defined for `{expected}`, evaluated with `{found}`"
      ),
      Self::Evaluation(msg) => write!(f, "policy evaluation failed: {msg}"),
    }
  }
}

impl std::error::Error for AuthzError {}

/// The effect a policy evaluates to.
pub type PolicyEffect = Effect<Decision, AuthzError>;

/// A rule deciding whether a principal may act on a resource of type `Res` (`()` for rules that
/// look only at the principal).
///
/// The returned effect is what runs. A policy that only inspects its arguments may decide while
/// building it and return a ready effect; one that performs a lookup returns that lookup.
pub trait Policy<Res: ?Sized>: Send + Sync + 'static {
  /// Evaluate the policy for `principal` acting on `resource`.
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect;

  /// Whether the policy opted out of the automatic tenant match
  /// ([`PolicyExt::dangerous_cross_tenant`]); `false` for every other policy.
  fn crosses_tenants(&self) -> bool {
    false
  }
}

/// A shared, type-erased policy over `Res`.
pub type BoxPolicy<Res> = Arc<dyn Policy<Res>>;

impl<Res: ?Sized, P: Policy<Res> + ?Sized> Policy<Res> for Arc<P> {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    (**self).evaluate(principal, resource)
  }

  fn crosses_tenants(&self) -> bool {
    (**self).crosses_tenants()
  }
}

impl<Res: ?Sized, P: Policy<Res> + ?Sized> Policy<Res> for Box<P> {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    (**self).evaluate(principal, resource)
  }

  fn crosses_tenants(&self) -> bool {
    (**self).crosses_tenants()
  }
}

/// Combinators for policies.
///
/// The trait does not name the resource type: most policies ([`require_role`], [`allow_all`],
/// `Rbac::require`, claim policies) apply to every resource, and the combination is a
/// [`Policy<Res>`](Policy) for each `Res` both sides accept, fixed where it is used.
pub trait PolicyExt: Sized + Send + Sync + 'static {
  /// Allows when both allow; `other` runs only when `self` allows.
  fn and<P>(self, other: P) -> And<Self, P> {
    And(self, other)
  }

  /// Allows when either allows; `other` runs only when `self` denies (its denial is kept).
  fn or<P>(self, other: P) -> Or<Self, P> {
    Or(self, other)
  }

  /// Allows when `self` denies; denies with `reason` when it allows.
  fn negate(self, reason: impl Into<String>) -> Not<Self> {
    Not(self, reason.into())
  }

  /// This policy over `Res`, shared and type-erased.
  fn boxed<Res: ?Sized>(self) -> BoxPolicy<Res>
  where
    Self: Policy<Res>,
  {
    Arc::new(self)
  }

  /// This policy without the automatic tenant match: defined by name or run with
  /// [`check_with`] / [`authorize_with`], it decides on a [`Tenanted`](crate::Tenanted) resource
  /// for principals of any tenant. `reason` says who needs that (support tooling, a platform
  /// operator) and is kept on the policy for review.
  ///
  /// A dangerous opt-out (ADR 0013 R10): the call records
  /// `id_effect_authz::PolicyExt::dangerous_cross_tenant` in the posture registry. It applies to
  /// the policy it wraps as a whole, so it has to be the outermost combinator.
  fn dangerous_cross_tenant(self, reason: impl Into<String>) -> CrossTenant<Self> {
    record_dangerous(
      "id_effect_authz::PolicyExt::dangerous_cross_tenant",
      "M19: a policy over tenant-scoped resources decides for principals of other tenants",
    );
    CrossTenant {
      policy: self,
      reason: reason.into(),
    }
  }
}

impl<P: Sized + Send + Sync + 'static> PolicyExt for P {}

/// See [`PolicyExt::dangerous_cross_tenant`].
#[derive(Clone, Debug)]
pub struct CrossTenant<P> {
  policy: P,
  reason: String,
}

impl<P> CrossTenant<P> {
  /// Why the policy crosses tenants.
  pub fn reason(&self) -> &str {
    &self.reason
  }
}

impl<Res: ?Sized, P: Policy<Res>> Policy<Res> for CrossTenant<P> {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    self.policy.evaluate(principal, resource)
  }

  fn crosses_tenants(&self) -> bool {
    true
  }
}

/// See [`PolicyExt::and`].
#[derive(Clone, Debug)]
pub struct And<A, B>(A, B);

impl<Res: ?Sized, A: Policy<Res>, B: Policy<Res>> Policy<Res> for And<A, B> {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    let second = self.1.evaluate(principal, resource);
    self
      .0
      .evaluate(principal, resource)
      .flat_map(move |first| match first {
        Decision::Allow => second,
        denied => succeed(denied),
      })
  }
}

/// See [`PolicyExt::or`].
#[derive(Clone, Debug)]
pub struct Or<A, B>(A, B);

impl<Res: ?Sized, A: Policy<Res>, B: Policy<Res>> Policy<Res> for Or<A, B> {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    let second = self.1.evaluate(principal, resource);
    self
      .0
      .evaluate(principal, resource)
      .flat_map(move |first| match first {
        Decision::Allow => succeed(Decision::Allow),
        Decision::Deny(_) => second,
      })
  }
}

/// See [`PolicyExt::negate`].
#[derive(Clone, Debug)]
pub struct Not<A>(A, String);

impl<Res: ?Sized, A: Policy<Res>> Policy<Res> for Not<A> {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    let reason = self.1.clone();
    self
      .0
      .evaluate(principal, resource)
      .map(move |inner| match inner {
        Decision::Allow => Decision::deny(reason),
        Decision::Deny(_) => Decision::Allow,
      })
  }
}

/// Allows when every policy allows (in order, stopping at the first denial); an empty list
/// allows.
pub fn all_of<Res: ?Sized + 'static>(policies: Vec<BoxPolicy<Res>>) -> AllOf<Res> {
  AllOf(policies)
}

/// See [`all_of`].
pub struct AllOf<Res: ?Sized>(Vec<BoxPolicy<Res>>);

impl<Res: ?Sized + 'static> Policy<Res> for AllOf<Res> {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    let steps: Vec<PolicyEffect> = self
      .0
      .iter()
      .map(|p| p.evaluate(principal, resource))
      .collect();
    steps
      .into_iter()
      .rev()
      .fold(succeed(Decision::Allow), |rest, step| {
        step.flat_map(move |d| match d {
          Decision::Allow => rest,
          denied => succeed(denied),
        })
      })
  }
}

/// Allows when any policy allows (in order, stopping at the first allow); when all deny, the
/// last denial is kept; an empty list denies.
pub fn any_of<Res: ?Sized + 'static>(policies: Vec<BoxPolicy<Res>>) -> AnyOf<Res> {
  AnyOf(policies)
}

/// See [`any_of`].
pub struct AnyOf<Res: ?Sized>(Vec<BoxPolicy<Res>>);

impl<Res: ?Sized + 'static> Policy<Res> for AnyOf<Res> {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    let mut steps: Vec<PolicyEffect> = self
      .0
      .iter()
      .map(|p| p.evaluate(principal, resource))
      .collect();
    let Some(last) = steps.pop() else {
      return succeed(Decision::deny("no policy allowed"));
    };
    steps.into_iter().rev().fold(last, |rest, step| {
      step.flat_map(move |d| match d {
        Decision::Allow => succeed(Decision::Allow),
        Decision::Deny(_) => rest,
      })
    })
  }
}

impl<Res: ?Sized> fmt::Debug for AllOf<Res> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("AllOf").field(&self.0.len()).finish()
  }
}

impl<Res: ?Sized> fmt::Debug for AnyOf<Res> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("AnyOf").field(&self.0.len()).finish()
  }
}

/// A policy that always allows.
pub fn allow_all() -> Constant {
  Constant(Decision::Allow)
}

/// A policy that always denies with `reason`.
pub fn deny_all(reason: impl Into<String>) -> Constant {
  Constant(Decision::deny(reason))
}

/// See [`allow_all`] and [`deny_all`].
#[derive(Clone, Debug)]
pub struct Constant(Decision);

impl<Res: ?Sized> Policy<Res> for Constant {
  fn evaluate(&self, _: &CurrentPrincipal, _: &Res) -> PolicyEffect {
    succeed(self.0.clone())
  }
}

/// Allows any authenticated principal (denies [`Anonymous`](crate::Anonymous)).
pub fn authenticated() -> Authenticated {
  Authenticated
}

/// See [`authenticated`].
#[derive(Clone, Copy, Debug)]
pub struct Authenticated;

impl<Res: ?Sized> Policy<Res> for Authenticated {
  fn evaluate(&self, principal: &CurrentPrincipal, _: &Res) -> PolicyEffect {
    succeed(Decision::allow_if(
      principal.is_authenticated(),
      "authentication required",
    ))
  }
}

/// Allows a principal holding `role` directly (use [`Rbac::require_role`](crate::Rbac::require_role)
/// to honour role inheritance).
pub fn require_role(role: impl Into<String>) -> RequireRole {
  RequireRole(vec![role.into()])
}

/// Allows a principal holding at least one of `roles` directly.
pub fn require_any_role<S: Into<String>>(roles: impl IntoIterator<Item = S>) -> RequireRole {
  RequireRole(roles.into_iter().map(Into::into).collect())
}

/// See [`require_role`] and [`require_any_role`].
#[derive(Clone, Debug)]
pub struct RequireRole(Vec<String>);

impl<Res: ?Sized> Policy<Res> for RequireRole {
  fn evaluate(&self, principal: &CurrentPrincipal, _: &Res) -> PolicyEffect {
    if self.0.iter().any(|r| principal.has_role(r)) {
      succeed(Decision::Allow)
    } else {
      succeed(Decision::deny(format!(
        "requires role {}",
        self.0.join(" or ")
      )))
    }
  }
}

/// A policy from a synchronous predicate: allows when `check(principal, resource)` holds, else
/// denies with `reason`.
pub fn predicate<Res, F>(reason: impl Into<String>, check: F) -> PredicatePolicy<F>
where
  Res: ?Sized,
  F: Fn(&CurrentPrincipal, &Res) -> bool + Send + Sync + 'static,
{
  PredicatePolicy {
    reason: reason.into(),
    check,
  }
}

/// See [`predicate`].
#[derive(Clone)]
pub struct PredicatePolicy<F> {
  reason: String,
  check: F,
}

impl<F> fmt::Debug for PredicatePolicy<F> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("PredicatePolicy")
      .field("reason", &self.reason)
      .finish()
  }
}

impl<Res, F> Policy<Res> for PredicatePolicy<F>
where
  Res: ?Sized,
  F: Fn(&CurrentPrincipal, &Res) -> bool + Send + Sync + 'static,
{
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    succeed(Decision::allow_if(
      (self.check)(principal, resource),
      self.reason.as_str(),
    ))
  }
}

/// A policy from a function returning the decision effect (for policies that look something
/// up). Copy what the effect needs out of `resource`: the effect outlives the borrow.
pub fn from_effect<Res, F>(evaluate: F) -> EffectPolicy<F>
where
  Res: ?Sized,
  F: Fn(&CurrentPrincipal, &Res) -> PolicyEffect + Send + Sync + 'static,
{
  EffectPolicy(evaluate)
}

/// See [`from_effect`].
#[derive(Clone)]
pub struct EffectPolicy<F>(F);

impl<F> fmt::Debug for EffectPolicy<F> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("EffectPolicy")
  }
}

impl<Res, F> Policy<Res> for EffectPolicy<F>
where
  Res: ?Sized,
  F: Fn(&CurrentPrincipal, &Res) -> PolicyEffect + Send + Sync + 'static,
{
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    (self.0)(principal, resource)
  }
}

/// Evaluates `policy` for the [`CurrentPrincipal`] in `R` acting on `resource`, behind the
/// tenant check ([`TenantScope`]) unless the policy
/// [crosses tenants](PolicyExt::dangerous_cross_tenant).
pub fn check_with<P, Res, R, I>(policy: P, resource: Res) -> Effect<Decision, AuthzError, R>
where
  P: Policy<Res>,
  Res: TenantScope + Send + Sync + 'static,
  R: Selector<CurrentPrincipal, I> + HList,
{
  let policy = scoped(policy);
  need::<CurrentPrincipal>().flat_map(move |p| policy.evaluate(&p, &resource).narrow())
}

/// Like [`check_with`], failing with [`AuthzError::Denied`] (or
/// [`AuthzError::Unauthenticated`]) instead of returning a denial.
pub fn authorize_with<P, Res, R, I>(policy: P, resource: Res) -> Effect<(), AuthzError, R>
where
  P: Policy<Res>,
  Res: TenantScope + Send + Sync + 'static,
  R: Selector<CurrentPrincipal, I> + HList,
{
  let policy = scoped(policy);
  need::<CurrentPrincipal>().flat_map(move |p| {
    policy
      .evaluate(&p, &resource)
      .flat_map(move |d| into_effect(d, &p))
      .narrow()
  })
}

/// The decision as an effect that fails when it denies.
pub(crate) fn into_effect(
  decision: Decision,
  principal: &CurrentPrincipal,
) -> Effect<(), AuthzError> {
  match decision.into_result(principal) {
    Ok(()) => succeed(()),
    Err(e) => id_effect::fail(e),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::principal::{BasicPrincipal, principal_env};
  use id_effect::{Cause, Exit, fail};

  fn alice() -> CurrentPrincipal {
    CurrentPrincipal::new(BasicPrincipal::new("alice").with_role("accountant"))
  }

  fn decide<P: Policy<()>>(policy: &P, who: &CurrentPrincipal) -> Decision {
    match policy.evaluate(who, &()).block_on() {
      Exit::Success(d) => d,
      other => panic!("unexpected exit: {other:?}"),
    }
  }

  #[test]
  fn constants_and_authenticated() {
    assert_eq!(decide(&allow_all(), &alice()), Decision::Allow);
    assert_eq!(
      decide(&deny_all("closed"), &alice()),
      Decision::deny("closed")
    );
    assert!(decide(&authenticated(), &alice()).is_allowed());
    assert!(decide(&authenticated(), &CurrentPrincipal::anonymous()).is_denied());
  }

  #[test]
  fn roles_must_be_held() {
    assert!(decide(&require_role("accountant"), &alice()).is_allowed());
    let Decision::Deny(denial) = decide(&require_any_role(["owner", "admin"]), &alice()) else {
      panic!("expected a denial")
    };
    assert_eq!(denial.reason(), "requires role owner or admin");
    assert_eq!(denial.policy(), None);
  }

  #[test]
  fn and_or_not_short_circuit() {
    let yes = allow_all();
    let no = deny_all("no");
    assert!(decide(&yes.clone().and(yes.clone()), &alice()).is_allowed());
    assert_eq!(
      decide(&no.clone().and(yes.clone()), &alice()),
      Decision::deny("no")
    );
    assert_eq!(
      decide(&yes.clone().and(deny_all("second")), &alice()),
      Decision::deny("second")
    );
    assert!(decide(&no.clone().or(yes.clone()), &alice()).is_allowed());
    assert!(decide(&yes.clone().or(no.clone()), &alice()).is_allowed());
    assert_eq!(
      decide(&no.clone().or(deny_all("last")), &alice()),
      Decision::deny("last")
    );
    assert_eq!(
      decide(&yes.negate("inverted"), &alice()),
      Decision::deny("inverted")
    );
    assert!(decide(&no.negate("unused"), &alice()).is_allowed());
  }

  #[test]
  fn all_of_and_any_of() {
    let both = all_of(vec![
      allow_all().boxed(),
      require_role("accountant").boxed(),
    ]);
    assert!(decide(&both, &alice()).is_allowed());
    let one_denies = all_of(vec![
      allow_all().boxed(),
      deny_all("x").boxed(),
      deny_all("y").boxed(),
    ]);
    assert_eq!(decide(&one_denies, &alice()), Decision::deny("x"));
    assert!(decide(&all_of::<()>(vec![]), &alice()).is_allowed());
    let either = any_of(vec![deny_all("a").boxed(), allow_all().boxed()]);
    assert!(decide(&either, &alice()).is_allowed());
    let none = any_of(vec![deny_all("a").boxed(), deny_all("b").boxed()]);
    assert_eq!(decide(&none, &alice()), Decision::deny("b"));
    assert_eq!(
      decide(&any_of::<()>(vec![]), &alice()),
      Decision::deny("no policy allowed")
    );
    assert_eq!(format!("{both:?} {none:?}"), "AllOf(2) AnyOf(2)");
  }

  #[test]
  fn predicate_and_effect_policies() {
    let even = predicate("odd", |_: &CurrentPrincipal, n: &u32| n.is_multiple_of(2));
    assert!(even.evaluate(&alice(), &4).block_on() == Exit::Success(Decision::Allow));
    assert_eq!(
      even.evaluate(&alice(), &3).block_on(),
      Exit::Success(Decision::deny("odd"))
    );
    assert!(format!("{even:?}").contains("odd"));

    let lookup = from_effect(|p: &CurrentPrincipal, _: &()| {
      let id = p.id().to_owned();
      id_effect::sync(move || Decision::allow_if(id == "alice", "not alice"))
    });
    assert!(decide(&lookup, &alice()).is_allowed());
    assert_eq!(format!("{lookup:?}"), "EffectPolicy");

    let broken =
      from_effect(|_: &CurrentPrincipal, _: &()| fail(AuthzError::Evaluation("db down".into())));
    assert_eq!(
      broken.evaluate(&alice(), &()).block_on(),
      Exit::Failure(Cause::Fail(AuthzError::Evaluation("db down".into())))
    );
  }

  #[test]
  fn shared_policies_delegate() {
    let shared: Arc<RequireRole> = Arc::new(require_role("accountant"));
    assert!(decide(&shared, &alice()).is_allowed());
    let boxed: Box<dyn Policy<()>> = Box::new(deny_all("boxed"));
    assert_eq!(decide(&boxed, &alice()), Decision::deny("boxed"));
  }

  #[test]
  fn decisions_and_denials() {
    assert!(Decision::allow_if(true, "x").is_allowed());
    let d = Denial::new("nope")
      .attributed_to("p")
      .attributed_to("ignored");
    assert_eq!(d.policy(), Some("p"));
    assert_eq!(d.to_string(), "p: nope");
    assert_eq!(Denial::new("bare").to_string(), "bare");
    assert_eq!(Decision::Allow.into_result(&alice()), Ok(()));
    assert_eq!(
      Decision::deny("no").into_result(&alice()),
      Err(AuthzError::Denied(Denial::new("no")))
    );
    assert_eq!(
      Decision::deny("no").into_result(&CurrentPrincipal::anonymous()),
      Err(AuthzError::Unauthenticated)
    );
  }

  #[test]
  fn errors_display_and_map_to_http_status() {
    let cases = [
      (AuthzError::Unauthenticated, 401, "authentication required"),
      (AuthzError::Denied(Denial::new("r")), 403, "forbidden: r"),
      (
        AuthzError::UnknownPolicy("p".into()),
        500,
        "unknown policy `p`",
      ),
      (
        AuthzError::ResourceMismatch {
          policy: "p".into(),
          expected: "A",
          found: "B",
        },
        500,
        "policy `p` is defined for `A`, evaluated with `B`",
      ),
      (
        AuthzError::Evaluation("io".into()),
        500,
        "policy evaluation failed: io",
      ),
    ];
    for (err, status, text) in cases {
      assert_eq!(err.http_status(), status);
      assert_eq!(err.to_string(), text);
    }
  }

  #[test]
  fn check_and_authorize_read_the_principal_capability() {
    let env = || principal_env(BasicPrincipal::new("alice").with_role("accountant"));
    let checked = check_with(require_role("owner"), ()).provide_all(env());
    assert_eq!(
      checked.block_on(),
      Exit::Success(Decision::deny("requires role owner"))
    );
    let ok = authorize_with(require_role("accountant"), ()).provide_all(env());
    assert_eq!(ok.block_on(), Exit::Success(()));
    let denied = authorize_with(require_role("owner"), ()).provide_all(env());
    assert_eq!(
      denied.block_on(),
      Exit::Failure(Cause::Fail(AuthzError::Denied(Denial::new(
        "requires role owner"
      ))))
    );
    let anon = authorize_with(authenticated(), ()).provide_all(principal_env(crate::Anonymous));
    assert_eq!(
      anon.block_on(),
      Exit::Failure(Cause::Fail(AuthzError::Unauthenticated))
    );
  }
}
