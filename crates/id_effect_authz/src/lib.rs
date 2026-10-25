//! Authorization for id_effect services: policies are capabilities.
//!
//! - **Who** — the [`Principal`] trait (implemented by `id_effect_auth`'s principals and by
//!   [`BasicPrincipal`]), installed per request or job as the [`CurrentPrincipal`] capability.
//! - **Rules** — a [`Policy<Res>`](Policy) evaluates the principal against a resource as an
//!   effect returning a [`Decision`]; policies compose ([`PolicyExt`], [`all_of`], [`any_of`]).
//! - **Roles and permissions** — [`Rbac`]: role → permission table with inheritance and
//!   `area:*` wildcards ([`Rbac::require`], [`has_permission`]).
//! - **Resources** — ownership ([`owner`]), tenant match ([`same_tenant`]) and access lists
//!   ([`Acl`], [`acl_permits`]).
//! - **Tenant isolation** — automatic: every policy defined for a [`Tenanted`] resource, or run
//!   on one with [`check_with`] / [`authorize_with`], first requires the principal's tenant to be
//!   the resource's ([`TenantScope`]); crossing tenants is the recorded opt-out
//!   [`PolicyExt::dangerous_cross_tenant`]. Resource types of no tenant declare so with
//!   `impl TenantScope for T {}`.
//! - **Attributes** — claim predicates ([`claim_equals`], [`claim_contains`], [`claim`],
//!   [`claim_matches`]).
//! - **Named policies** — [`Policies`], a registry capability; [`authorize`] and [`check`]
//!   evaluate a rule by name for the principal in the environment.
//!
//! A denial is a value ([`Decision::Deny`] with a [`Denial`] reason) until
//! [`authorize`] / [`authorize_with`] turn it into an [`AuthzError`] whose
//! [`http_status`](AuthzError::http_status) is 401 for an anonymous principal and 403 otherwise.
//!
//! ```
//! use id_effect::{Effect, Exit, HCons, HNil, hlist};
//! use id_effect_authz::{
//!   AuthzError, BasicPrincipal, CurrentPrincipal, Policies, Rbac, authorize,
//! };
//!
//! let rbac = Rbac::builder().grant("accountant", ["ledger:post"]).build();
//! let policies = Policies::builder().define::<()>("ledger.post", rbac.require("ledger:post")).build();
//!
//! fn post_entry() -> Effect<(), AuthzError, HCons<CurrentPrincipal, HCons<Policies, HNil>>> {
//!   authorize("ledger.post", ())
//! }
//!
//! let carol = CurrentPrincipal::new(BasicPrincipal::new("carol").with_role("accountant"));
//! assert_eq!(post_entry().provide_all(hlist![carol, policies]).block_on(), Exit::Success(()));
//! ```
//!
//! - **Decision events** — every decision can be recorded as a serialisable [`DecisionEvent`] to
//!   the [`AuthzEvents`] sink capability ([`PoliciesBuilder::events`], [`AuthzEvents::observe`]).
//! - **Route guards** (feature `axum`) — `guard::Guard`, a Tower layer answering 401/403 with
//!   Problem Details, attached with `guard::AuthorizeExt::authorize`.
//! - **Test helpers** (feature `testing`) — `testing::AuthzTest` with `assert_allows` /
//!   `assert_denies` and the decisions it recorded.
//!
//! Tier `platform` (ADR 0010 Amendment 5): the Ledger reference app under
//! `examples/reference/` depends on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod attr;
mod audit;
#[cfg(feature = "axum")]
pub mod guard;
mod policy;
mod principal;
mod rbac;
mod registry;
mod resource;
pub mod tenant;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use attr::{
  ClaimCheck, ClaimContains, ClaimEquals, ClaimMatches, ClaimPolicy, claim, claim_contains,
  claim_equals, claim_matches,
};
pub use audit::{
  AuthzEvents, AuthzEventsEnv, AuthzSink, DecisionEvent, MemorySink, Observed, Outcome,
  emit_decision, provide_authz_events,
};
#[cfg(feature = "axum")]
pub use guard::{AuthorizeExt, AuthorizedEndpoint, Guard, GuardService};
pub use policy::{
  AllOf, And, AnyOf, Authenticated, AuthzError, BoxPolicy, Constant, CrossTenant, Decision, Denial,
  EffectPolicy, Not, Or, Policy, PolicyEffect, PolicyExt, PredicatePolicy, RequireRole, all_of,
  allow_all, any_of, authenticated, authorize_with, check_with, deny_all, from_effect, predicate,
  require_any_role, require_role,
};
pub use principal::{
  Anonymous, BasicPrincipal, CurrentPrincipal, Principal, PrincipalEnv, current_principal,
  principal_env, provide_principal,
};
pub use rbac::{
  Rbac, RbacBuilder, RbacEnv, RequireInheritedRole, RequirePermission, has_permission,
  permission_matches, provide_rbac,
};
pub use registry::{Policies, PoliciesBuilder, PoliciesEnv, authorize, check, provide_policies};
pub use resource::{
  Acl, AclPolicy, Grantee, HasAcl, Owned, OwnerPolicy, SameTenant, TenantScope, TenantScoped,
  Tenanted, acl_permits, owner, same_tenant, tenant_scoped,
};
pub use tenant::{TenantEnv, TenantId, TenantResolver, current_tenant, tenant_env};
#[cfg(feature = "axum")]
pub use tenant::{TenantLayer, TenantService};

/// Compiles and runs the README's code block as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
