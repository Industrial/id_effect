//! Who is asking: the [`Principal`] trait and the [`CurrentPrincipal`] capability.
//!
//! An authentication layer (`id_effect_auth`, a test, an API-key check) turns a request into a
//! [`Principal`] and installs it as the [`CurrentPrincipal`] capability, typically per request
//! with `id_effect_axum::CapOverrideLayer`. Policies read it from there.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect::{Effect, HCons, HNil};
//! use id_effect_authz::{BasicPrincipal, CurrentPrincipal, current_principal};
//!
//! fn who_am_i() -> Effect<String, (), HCons<CurrentPrincipal, HNil>> {
//!   current_principal().map(|p: CurrentPrincipal| p.id().to_owned())
//! }
//!
//! let alice = BasicPrincipal::new("alice").with_role("accountant");
//! let who = who_am_i().provide(CurrentPrincipal::new(alice));
//! assert_eq!(who.block_on(), Exit::Success("alice".to_owned()));
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, hlist, need};
use serde_json::Value;

/// An authenticated (or anonymous) subject: its id, roles, direct permissions and claims.
///
/// `id_effect_auth` implements it for its session and token principals; [`BasicPrincipal`] is
/// a plain implementation for API keys, service accounts and tests. Only [`id`](Principal::id)
/// and [`roles`](Principal::roles) are required.
pub trait Principal: Send + Sync + 'static {
  /// Stable subject id (user id, service account, `sub` claim).
  fn id(&self) -> &str;

  /// Roles held directly (`Rbac` expands inherited roles).
  fn roles(&self) -> &[String];

  /// Permissions granted directly, besides those of the roles (OAuth scopes, API-key grants).
  fn permissions(&self) -> &[String] {
    &[]
  }

  /// The claim or attribute `name` (JWT/OIDC claim, profile attribute), if present.
  fn claim(&self, name: &str) -> Option<&Value> {
    let _ = name;
    None
  }

  /// The tenant the principal acts for, if the application is multi-tenant.
  fn tenant(&self) -> Option<&str> {
    None
  }

  /// `false` for the anonymous principal: a denial then means "log in" (401), not "forbidden".
  fn is_authenticated(&self) -> bool {
    true
  }

  /// Whether `role` is held directly.
  fn has_role(&self, role: &str) -> bool {
    self.roles().iter().any(|r| r == role)
  }
}

/// A [`Principal`] built from plain values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BasicPrincipal {
  id: String,
  roles: Vec<String>,
  permissions: Vec<String>,
  claims: BTreeMap<String, Value>,
  tenant: Option<String>,
}

impl BasicPrincipal {
  /// A principal with id `id` and nothing else.
  pub fn new(id: impl Into<String>) -> Self {
    Self {
      id: id.into(),
      ..Self::default()
    }
  }

  /// Adds the role `role`.
  #[must_use]
  pub fn with_role(mut self, role: impl Into<String>) -> Self {
    self.roles.push(role.into());
    self
  }

  /// Adds every role in `roles`.
  #[must_use]
  pub fn with_roles<S: Into<String>>(mut self, roles: impl IntoIterator<Item = S>) -> Self {
    self.roles.extend(roles.into_iter().map(Into::into));
    self
  }

  /// Adds the direct permission `permission`.
  #[must_use]
  pub fn with_permission(mut self, permission: impl Into<String>) -> Self {
    self.permissions.push(permission.into());
    self
  }

  /// Sets the claim `name` to `value`.
  #[must_use]
  pub fn with_claim(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
    self.claims.insert(name.into(), value.into());
    self
  }

  /// Sets the tenant.
  #[must_use]
  pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
    self.tenant = Some(tenant.into());
    self
  }
}

impl Principal for BasicPrincipal {
  fn id(&self) -> &str {
    &self.id
  }

  fn roles(&self) -> &[String] {
    &self.roles
  }

  fn permissions(&self) -> &[String] {
    &self.permissions
  }

  fn claim(&self, name: &str) -> Option<&Value> {
    self.claims.get(name)
  }

  fn tenant(&self) -> Option<&str> {
    self.tenant.as_deref()
  }
}

/// The unauthenticated principal: no id, no roles, [`is_authenticated`](Principal::is_authenticated)
/// is `false`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Anonymous;

impl Principal for Anonymous {
  fn id(&self) -> &str {
    ""
  }

  fn roles(&self) -> &[String] {
    &[]
  }

  fn is_authenticated(&self) -> bool {
    false
  }
}

/// The [`Principal`] capability: a shared handle to the principal of the current request or job.
#[derive(Clone)]
pub struct CurrentPrincipal(pub Arc<dyn Principal>);

impl CurrentPrincipal {
  /// Wrap `principal` as the capability.
  #[inline]
  pub fn new(principal: impl Principal) -> Self {
    Self(Arc::new(principal))
  }

  /// The [`Anonymous`] principal.
  #[inline]
  pub fn anonymous() -> Self {
    Self::new(Anonymous)
  }
}

impl std::ops::Deref for CurrentPrincipal {
  type Target = dyn Principal;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl fmt::Debug for CurrentPrincipal {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CurrentPrincipal")
      .field("id", &self.id())
      .field("authenticated", &self.is_authenticated())
      .finish()
  }
}

/// The minimal typed environment holding only [`CurrentPrincipal`].
pub type PrincipalEnv = HCons<CurrentPrincipal, HNil>;

/// A [`PrincipalEnv`] wrapping `principal` (for `.provide_all(..)` in tests and entry points).
#[inline]
pub fn principal_env(principal: impl Principal) -> PrincipalEnv {
  hlist![CurrentPrincipal::new(principal)]
}

/// A [`Layer`] providing `principal` as the [`CurrentPrincipal`] capability (a job's service
/// account, a test user). Per-request principals come from the authentication middleware.
#[inline]
pub fn provide_principal<E: Send + 'static>(principal: impl Principal) -> Layer<PrincipalEnv, E> {
  Layer::succeed(principal_env(principal))
}

/// The [`CurrentPrincipal`] in `R`.
#[inline]
pub fn current_principal<E, R, I>() -> Effect<CurrentPrincipal, E, R>
where
  E: Send + 'static,
  R: Selector<CurrentPrincipal, I> + HList,
{
  need::<CurrentPrincipal>().map(|p| CurrentPrincipal::clone(&p))
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Exit;
  use serde_json::json;

  #[test]
  fn basic_principal_exposes_what_it_was_built_with() {
    let p = BasicPrincipal::new("alice")
      .with_role("viewer")
      .with_roles(["accountant", "auditor"])
      .with_permission("report:export")
      .with_claim("department", "finance")
      .with_tenant("acme");
    assert_eq!(p.id(), "alice");
    assert_eq!(p.roles(), ["viewer", "accountant", "auditor"]);
    assert_eq!(p.permissions(), ["report:export"]);
    assert_eq!(p.claim("department"), Some(&json!("finance")));
    assert_eq!(p.claim("missing"), None);
    assert_eq!(p.tenant(), Some("acme"));
    assert!(p.is_authenticated());
    assert!(p.has_role("auditor"));
    assert!(!p.has_role("owner"));
  }

  #[test]
  fn anonymous_is_unauthenticated_and_empty() {
    let p = Anonymous;
    assert_eq!(p.id(), "");
    assert!(p.roles().is_empty());
    assert!(p.permissions().is_empty());
    assert_eq!(p.claim("sub"), None);
    assert_eq!(p.tenant(), None);
    assert!(!p.is_authenticated());
  }

  #[test]
  fn current_principal_derefs_and_debugs() {
    let cur = CurrentPrincipal::new(BasicPrincipal::new("bob"));
    assert_eq!(cur.id(), "bob");
    let dbg = format!("{cur:?}");
    assert!(
      dbg.contains("bob") && dbg.contains("authenticated: true"),
      "{dbg}"
    );
    assert!(!CurrentPrincipal::anonymous().is_authenticated());
  }

  #[test]
  fn layer_and_env_provide_the_capability() {
    let who: Effect<String, (), PrincipalEnv> =
      current_principal().map(|p: CurrentPrincipal| p.id().to_owned());
    let via_env = current_principal::<(), _, _>()
      .map(|p| p.id().to_owned())
      .provide_all(principal_env(BasicPrincipal::new("env")));
    assert_eq!(via_env.block_on(), Exit::Success("env".to_owned()));
    let via_layer = who.provide_layer(provide_principal(BasicPrincipal::new("layer")));
    assert_eq!(via_layer.block_on(), Exit::Success("layer".to_owned()));
  }
}
