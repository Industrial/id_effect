//! Who may join a topic: the [`JoinPolicy`] every join goes through (ADR 0013 R1, ADR 0014
//! C11). The default, [`DenyAllJoins`], refuses everyone; an application states its rule with a
//! closure ([`join_policy_fn`]), with an `id_effect_authz` policy ([`AuthzJoin`]), or — recorded
//! as a dangerous opt-out — lets everyone in ([`dangerous_allow_all_joins`]).
//!
//! The policy decides per join, on every subscribe and every resume (each is a new call), with
//! the joining principal, the topic and the metadata the connection asked to join with. A denial
//! reaches the caller as `{"_tag": "JoinDenied"}` (403) or, for an anonymous principal,
//! `{"_tag": "Unauthenticated"}` (401): the policy's reason stays in the server log.

use std::fmt;
use std::sync::Arc;

use id_effect::{DynEnv, Effect, Exit, Runtime, die, fail, from_async, succeed};
use id_effect_authz::{AuthzError, CurrentPrincipal, Decision, Policy, TenantScope};
use serde_json::Value;

/// Posture id of [`dangerous_allow_all_joins`].
pub const DANGEROUS_ALLOW_ALL_JOINS: &str = "id_effect_rpc::channels::dangerous_allow_all_joins";

/// One join, as the policy sees it.
#[derive(Clone)]
pub struct JoinRequest {
  /// The topic asked for.
  pub topic: String,
  /// The joining principal (anonymous when the call carries none).
  pub principal: CurrentPrincipal,
  /// The metadata the connection joins with.
  pub meta: Value,
}

impl fmt::Debug for JoinRequest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("JoinRequest")
      .field("topic", &self.topic)
      .field("principal", &self.principal.id())
      .finish_non_exhaustive()
  }
}

/// Decides whether a join may proceed. The effect runs with the call's environment, so a policy
/// can look membership up (a room's members in the database, say).
pub trait JoinPolicy: Send + Sync + 'static {
  /// The decision for `join`. A failure ends the join as an internal error.
  fn decide(&self, join: &JoinRequest) -> Effect<Decision, AuthzError, DynEnv>;
}

impl<P: JoinPolicy + ?Sized> JoinPolicy for Arc<P> {
  fn decide(&self, join: &JoinRequest) -> Effect<Decision, AuthzError, DynEnv> {
    (**self).decide(join)
  }
}

/// The default policy: every join is denied.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAllJoins;

impl JoinPolicy for DenyAllJoins {
  fn decide(&self, _: &JoinRequest) -> Effect<Decision, AuthzError, DynEnv> {
    succeed(Decision::deny("no join policy is configured"))
  }
}

/// A policy that admits every join (see [`dangerous_allow_all_joins`]).
#[derive(Clone, Copy, Debug)]
pub struct AllowAllJoins(());

impl JoinPolicy for AllowAllJoins {
  fn decide(&self, _: &JoinRequest) -> Effect<Decision, AuthzError, DynEnv> {
    succeed(Decision::Allow)
  }
}

/// **Dangerous.** Admit every join, anonymous callers included. Recorded in the posture
/// registry as [`DANGEROUS_ALLOW_ALL_JOINS`]: any caller that can reach the procedure reads
/// every topic and appears in its presence.
pub fn dangerous_allow_all_joins() -> AllowAllJoins {
  id_effect::observability::posture::record_dangerous(
    DANGEROUS_ALLOW_ALL_JOINS,
    "web.realtime#6: every caller may join every channel topic (no join policy)",
  );
  AllowAllJoins(())
}

/// A policy from a synchronous rule.
///
/// ```rust
/// use id_effect_authz::{Decision, Principal};
/// use id_effect_rpc::channels::{JoinRequest, join_policy_fn};
///
/// // Rooms are open to their own members only: "room:<name>" needs the role "<name>".
/// let rooms = join_policy_fn(|join: &JoinRequest| {
///   let room = join.topic.strip_prefix("room:").unwrap_or_default();
///   Decision::allow_if(join.principal.has_role(room), "not a member of this room")
/// });
/// # let _ = rooms;
/// ```
pub fn join_policy_fn<F>(rule: F) -> FnJoinPolicy<F>
where
  F: Fn(&JoinRequest) -> Decision + Send + Sync + 'static,
{
  FnJoinPolicy(rule)
}

/// See [`join_policy_fn`].
#[derive(Clone, Copy)]
pub struct FnJoinPolicy<F>(F);

impl<F> fmt::Debug for FnJoinPolicy<F> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("FnJoinPolicy")
  }
}

impl<F> JoinPolicy for FnJoinPolicy<F>
where
  F: Fn(&JoinRequest) -> Decision + Send + Sync + 'static,
{
  fn decide(&self, join: &JoinRequest) -> Effect<Decision, AuthzError, DynEnv> {
    succeed((self.0)(join))
  }
}

/// The resource an [`AuthzJoin`] policy is evaluated on: the topic, and the tenant it belongs
/// to when [`AuthzJoin::with_tenant`] says so. A tenanted topic first requires the principal's
/// tenant to match (the `id_effect_authz` tenant check).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinTarget {
  /// The topic.
  pub topic: String,
  /// The topic's tenant, if it has one.
  pub tenant: Option<String>,
}

impl TenantScope for JoinTarget {
  fn tenant_scope(&self) -> Option<&str> {
    self.tenant.as_deref()
  }
}

type TenantOf = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// An `id_effect_authz` [`Policy`] over [`JoinTarget`] as a join policy (no axum items: the
/// policy traits of `id_effect_authz` without features).
///
/// ```rust
/// use id_effect_authz::require_role;
/// use id_effect_rpc::channels::AuthzJoin;
///
/// let members = AuthzJoin::new(require_role("member"))
///   // "acme/general" belongs to tenant "acme".
///   .with_tenant(|topic| topic.split_once('/').map(|(tenant, _)| tenant.to_string()));
/// # let _ = members;
/// ```
pub struct AuthzJoin<P> {
  policy: Arc<P>,
  tenant_of: TenantOf,
}

impl<P> Clone for AuthzJoin<P> {
  fn clone(&self) -> Self {
    Self {
      policy: Arc::clone(&self.policy),
      tenant_of: Arc::clone(&self.tenant_of),
    }
  }
}

impl<P> fmt::Debug for AuthzJoin<P> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("AuthzJoin")
  }
}

impl<P: Policy<JoinTarget>> AuthzJoin<P> {
  /// Join when `policy` allows the principal on the topic (topics have no tenant).
  pub fn new(policy: P) -> Self {
    Self {
      policy: Arc::new(policy),
      tenant_of: Arc::new(|_| None),
    }
  }

  /// Topics belong to the tenant `tenant_of` names (`None`: no tenant).
  #[must_use]
  pub fn with_tenant(
    mut self,
    tenant_of: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
  ) -> Self {
    self.tenant_of = Arc::new(tenant_of);
    self
  }
}

impl<P: Policy<JoinTarget>> JoinPolicy for AuthzJoin<P> {
  fn decide(&self, join: &JoinRequest) -> Effect<Decision, AuthzError, DynEnv> {
    let target = JoinTarget {
      topic: join.topic.clone(),
      tenant: (self.tenant_of)(&join.topic),
    };
    // The tenant check of `id_effect_authz` (`tenant_scoped`), decided before the policy runs.
    if let Some(tenant) = target.tenant_scope()
      && join.principal.tenant() != Some(tenant)
    {
      return succeed(Decision::deny("the topic belongs to another tenant"));
    }
    if let Some(decision) = self.policy.decide_now(&join.principal, &target) {
      return succeed(decision);
    }
    lift(self.policy.evaluate(&join.principal, &target))
  }
}

/// Run an environment-free policy effect inside the call's environment.
fn lift(effect: Effect<Decision, AuthzError>) -> Effect<Decision, AuthzError, DynEnv> {
  from_async(move || async move { Ok::<_, AuthzError>(Runtime::global().run(effect).await) })
    .flat_map(|exit| match exit {
      Exit::Success(decision) => succeed(decision),
      Exit::Failure(id_effect::Cause::Fail(error)) => fail(error),
      Exit::Failure(_) => die("the join policy did not decide"),
    })
}

/// The principal of a call: the [`CurrentPrincipal`] in its environment, else anonymous.
pub(crate) fn principal_of(env: &DynEnv) -> CurrentPrincipal {
  env
    .try_get::<CurrentPrincipal>()
    .map_or_else(|_| CurrentPrincipal::anonymous(), |p| (*p).clone())
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_authz::{BasicPrincipal, from_effect, require_role};
  use serde_json::json;

  fn join(topic: &str, principal: CurrentPrincipal) -> JoinRequest {
    JoinRequest {
      topic: topic.to_string(),
      principal,
      meta: json!({}),
    }
  }

  fn run(effect: Effect<Decision, AuthzError, DynEnv>) -> Exit<Decision, AuthzError> {
    effect.provide_all(DynEnv::new()).block_on()
  }

  fn alice() -> CurrentPrincipal {
    CurrentPrincipal::new(
      BasicPrincipal::new("alice")
        .with_role("member")
        .with_tenant("acme"),
    )
  }

  #[test]
  fn default_policy_denies_everyone() {
    let decision = run(DenyAllJoins.decide(&join("t", alice())));
    assert!(matches!(decision, Exit::Success(d) if d.is_denied()));
    assert!(format!("{:?}", join("t", alice())).contains("alice"));
  }

  #[test]
  fn allow_all_is_recorded() {
    let policy = dangerous_allow_all_joins();
    assert!(matches!(
      run(policy.decide(&join("t", alice()))),
      Exit::Success(Decision::Allow)
    ));
    assert!(
      id_effect::observability::posture::dangerous_opt_outs()
        .iter()
        .any(|(id, _)| *id == DANGEROUS_ALLOW_ALL_JOINS)
    );
  }

  #[test]
  fn fn_policy_and_arc_delegate() {
    let policy: Arc<dyn JoinPolicy> = Arc::new(join_policy_fn(|j: &JoinRequest| {
      Decision::allow_if(j.topic == "open", "closed")
    }));
    assert!(matches!(
      run(policy.decide(&join("open", alice()))),
      Exit::Success(Decision::Allow)
    ));
    assert!(
      matches!(run(policy.decide(&join("shut", alice()))), Exit::Success(d) if d.is_denied())
    );
    assert_eq!(
      format!("{:?}", join_policy_fn(|_| Decision::Allow)),
      "FnJoinPolicy"
    );
  }

  #[test]
  fn authz_policy_with_tenants() {
    let policy = AuthzJoin::new(require_role("member"))
      .with_tenant(|topic| topic.split_once('/').map(|(t, _)| t.to_string()));
    let copy = policy.clone();
    assert_eq!(format!("{copy:?}"), "AuthzJoin");
    assert!(matches!(
      run(policy.decide(&join("acme/general", alice()))),
      Exit::Success(Decision::Allow)
    ));
    assert!(matches!(
      run(policy.decide(&join("lobby", alice()))),
      Exit::Success(Decision::Allow)
    ));
    let other = run(policy.decide(&join("globex/general", alice())));
    assert!(
      matches!(other, Exit::Success(d) if d.is_denied()),
      "other tenant"
    );
    let anon = run(policy.decide(&join("lobby", CurrentPrincipal::anonymous())));
    assert!(matches!(anon, Exit::Success(d) if d.is_denied()));
  }

  #[test]
  fn effectful_authz_policies_run_and_fail() {
    let lookup = AuthzJoin::new(from_effect(|_: &CurrentPrincipal, t: &JoinTarget| {
      let open = t.topic == "open";
      id_effect::sync(move || Decision::allow_if(open, "closed"))
    }));
    assert!(matches!(
      run(lookup.decide(&join("open", alice()))),
      Exit::Success(Decision::Allow)
    ));
    let failing = AuthzJoin::new(from_effect(|_: &CurrentPrincipal, _: &JoinTarget| {
      id_effect::fail(AuthzError::UnknownPolicy("rooms".into()))
    }));
    assert!(matches!(
      run(failing.decide(&join("open", alice()))),
      Exit::Failure(id_effect::Cause::Fail(AuthzError::UnknownPolicy(_)))
    ));
    let dying = AuthzJoin::new(from_effect(|_: &CurrentPrincipal, _: &JoinTarget| {
      id_effect::die("boom")
    }));
    assert!(matches!(
      run(dying.decide(&join("open", alice()))),
      Exit::Failure(_)
    ));
  }

  #[test]
  fn principal_comes_from_the_environment() {
    assert!(!principal_of(&DynEnv::new()).is_authenticated());
    let env = DynEnv::new().with(alice());
    assert_eq!(principal_of(&env).id(), "alice");
  }
}
