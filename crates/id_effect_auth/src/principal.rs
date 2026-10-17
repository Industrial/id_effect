//! [`AuthPrincipal`]: the principal a credential authenticates, as an
//! [`id_effect_authz::Principal`].
//!
//! Every credential check of this crate (session, bearer JWT, API key, Basic password) yields an
//! [`AuthPrincipal`] recording the [`AuthScheme`] that produced it. The authentication layer
//! (feature `axum`) installs it as the request's [`CurrentPrincipal`](id_effect_authz::CurrentPrincipal),
//! where `id_effect_authz` policies and route guards read it.
//!
//! ```
//! use id_effect_auth::{AuthPrincipal, AuthScheme};
//! use id_effect_authz::Principal;
//!
//! let p = AuthPrincipal::new("alice")
//!   .with_role("accountant")
//!   .with_permission("ledger:read")
//!   .with_tenant("acme")
//!   .with_scheme(AuthScheme::Bearer);
//! assert_eq!(p.id(), "alice");
//! assert!(p.has_role("accountant"));
//! assert_eq!(p.scheme(), Some(AuthScheme::Bearer));
//! ```

use std::collections::BTreeMap;
use std::fmt;

use id_effect_authz::Principal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::Session;

/// Session attribute read by [`AuthPrincipal::from_session`] for the roles (space- or
/// comma-separated).
pub const SESSION_ROLES_ATTR: &str = "roles";
/// Session attribute read by [`AuthPrincipal::from_session`] for direct permissions (space- or
/// comma-separated).
pub const SESSION_PERMISSIONS_ATTR: &str = "permissions";
/// Session attribute read by [`AuthPrincipal::from_session`] for the tenant.
pub const SESSION_TENANT_ATTR: &str = "tenant";

/// How a request was authenticated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
  /// A server-side session cookie.
  Session,
  /// `Authorization: Bearer <JWT>`.
  Bearer,
  /// An API key header.
  ApiKey,
  /// `Authorization: Basic <user:password>`.
  Basic,
}

impl AuthScheme {
  /// The scheme's lower-case name (`session`, `bearer`, `api_key`, `basic`).
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Session => "session",
      Self::Bearer => "bearer",
      Self::ApiKey => "api_key",
      Self::Basic => "basic",
    }
  }
}

impl fmt::Display for AuthScheme {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// An authenticated subject: id, roles, direct permissions, claims, tenant and the
/// [`AuthScheme`] that authenticated it. Implements [`Principal`].
///
/// Serializable, so an API-key or user store can keep the principal a credential stands for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthPrincipal {
  id: String,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  roles: Vec<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  permissions: Vec<String>,
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  claims: BTreeMap<String, Value>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  tenant: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  scheme: Option<AuthScheme>,
}

impl AuthPrincipal {
  /// A principal with id `id` and nothing else.
  pub fn new(id: impl Into<String>) -> Self {
    Self {
      id: id.into(),
      ..Self::default()
    }
  }

  /// The principal of a server-side session: its user id, the roles, permissions and tenant
  /// stored in the attributes [`SESSION_ROLES_ATTR`], [`SESSION_PERMISSIONS_ATTR`] and
  /// [`SESSION_TENANT_ATTR`], and scheme [`AuthScheme::Session`]. Other attributes (a CSRF
  /// secret, say) are not exposed as claims.
  pub fn from_session(session: &Session) -> Self {
    let list = |key: &str| session.attr(key).map(split_list).unwrap_or_default();
    Self {
      id: session.user_id().to_owned(),
      roles: list(SESSION_ROLES_ATTR),
      permissions: list(SESSION_PERMISSIONS_ATTR),
      claims: BTreeMap::new(),
      tenant: session.attr(SESSION_TENANT_ATTR).map(str::to_owned),
      scheme: Some(AuthScheme::Session),
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

  /// Adds every permission in `permissions`.
  #[must_use]
  pub fn with_permissions<S: Into<String>>(
    mut self,
    permissions: impl IntoIterator<Item = S>,
  ) -> Self {
    self
      .permissions
      .extend(permissions.into_iter().map(Into::into));
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

  /// Records the scheme that authenticated the principal.
  #[must_use]
  pub fn with_scheme(mut self, scheme: AuthScheme) -> Self {
    self.scheme = Some(scheme);
    self
  }

  /// Every claim (for a JWT: the whole payload).
  #[inline]
  pub fn claims(&self) -> &BTreeMap<String, Value> {
    &self.claims
  }

  /// The scheme that authenticated the principal (`None` for one built by hand).
  #[inline]
  pub fn scheme(&self) -> Option<AuthScheme> {
    self.scheme
  }
}

impl Principal for AuthPrincipal {
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

/// `"a b,c"` → `["a", "b", "c"]` (space- or comma-separated, empty items dropped).
pub(crate) fn split_list(s: &str) -> Vec<String> {
  s.split([' ', ','])
    .filter(|item| !item.is_empty())
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
  use id_effect::UtcDateTime;
  use serde_json::json;

  use super::*;
  use crate::session::SessionId;

  #[test]
  fn builder_and_principal_view() {
    let p = AuthPrincipal::new("alice")
      .with_role("viewer")
      .with_roles(["accountant"])
      .with_permission("a:b")
      .with_permissions(["c:d"])
      .with_claim("email", "a@example.com")
      .with_tenant("acme")
      .with_scheme(AuthScheme::ApiKey);
    assert_eq!(p.id(), "alice");
    assert_eq!(p.roles(), ["viewer", "accountant"]);
    assert_eq!(p.permissions(), ["a:b", "c:d"]);
    assert_eq!(p.claim("email"), Some(&json!("a@example.com")));
    assert_eq!(p.claims().len(), 1);
    assert_eq!(Principal::tenant(&p), Some("acme"));
    assert_eq!(p.scheme(), Some(AuthScheme::ApiKey));
    assert!(p.is_authenticated());
    let back: AuthPrincipal = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
    assert_eq!(back, p);
    assert_eq!(
      serde_json::to_value(AuthPrincipal::new("x")).unwrap(),
      json!({"id": "x"})
    );
  }

  #[test]
  fn scheme_names() {
    let all = [
      (AuthScheme::Session, "session"),
      (AuthScheme::Bearer, "bearer"),
      (AuthScheme::ApiKey, "api_key"),
      (AuthScheme::Basic, "basic"),
    ];
    for (scheme, name) in all {
      assert_eq!(scheme.to_string(), name);
      assert_eq!(serde_json::to_value(scheme).unwrap(), json!(name));
    }
  }

  #[test]
  fn session_attributes_map_to_roles_permissions_and_tenant() {
    let t = UtcDateTime::unsafe_make(0);
    let mut s = Session::from_parts(
      SessionId::generate().unwrap(),
      "bob",
      BTreeMap::new(),
      t,
      t,
      t,
      t,
    );
    s.set_attr("roles", "admin, viewer")
      .set_attr("permissions", "x:y")
      .set_attr("tenant", "acme")
      .set_attr("csrf", "secret");
    let p = AuthPrincipal::from_session(&s);
    assert_eq!(p.id(), "bob");
    assert_eq!(p.roles(), ["admin", "viewer"]);
    assert_eq!(p.permissions(), ["x:y"]);
    assert_eq!(Principal::tenant(&p), Some("acme"));
    assert_eq!(p.claim("csrf"), None);
    assert_eq!(p.scheme(), Some(AuthScheme::Session));
  }
}
