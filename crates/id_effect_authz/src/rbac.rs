//! Role-based access control: [`Rbac`] maps roles to permissions, with role inheritance and
//! `area:*` / `*` wildcards.
//!
//! The table is a capability ([`RbacEnv`], [`provide_rbac`]) so handlers can ask
//! [`has_permission`], and it hands out policies ([`Rbac::require`], [`Rbac::require_role`]) for
//! the named-policy registry.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision, Policy, Rbac};
//!
//! let rbac = Rbac::builder()
//!   .grant("viewer", ["ledger:read"])
//!   .grant("accountant", ["ledger:post"])
//!   .inherit("accountant", "viewer")
//!   .grant("owner", ["ledger:*"])
//!   .build();
//!
//! let carol = BasicPrincipal::new("carol").with_role("accountant");
//! assert!(rbac.permits(&carol, "ledger:read")); // inherited from viewer
//! assert!(!rbac.permits(&carol, "ledger:close"));
//!
//! let close = rbac.require("ledger:close");
//! let decision = close.evaluate(&CurrentPrincipal::new(carol), &()).block_on();
//! assert!(matches!(decision, Exit::Success(Decision::Deny(_))));
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, hlist, need, succeed};

use crate::policy::{Decision, Policy, PolicyEffect};
use crate::principal::{CurrentPrincipal, Principal};

/// Whether the grant `granted` covers the permission `wanted`: equal, `*`, or a `prefix:*`
/// wildcard covering `prefix:anything`.
pub fn permission_matches(granted: &str, wanted: &str) -> bool {
  if granted == "*" || granted == wanted {
    return true;
  }
  granted
    .strip_suffix('*')
    .is_some_and(|prefix| prefix.ends_with(':') && wanted.starts_with(prefix))
}

#[derive(Debug, Default)]
struct Table {
  grants: BTreeMap<String, BTreeSet<String>>,
  parents: BTreeMap<String, BTreeSet<String>>,
}

/// A role → permission table with role inheritance. Cheap to clone (shared).
#[derive(Clone, Debug, Default)]
pub struct Rbac(Arc<Table>);

/// Builds an [`Rbac`] table.
#[derive(Debug, Default)]
pub struct RbacBuilder(Table);

impl RbacBuilder {
  /// Grants every permission in `permissions` to `role`.
  #[must_use]
  pub fn grant<S: Into<String>>(
    mut self,
    role: impl Into<String>,
    permissions: impl IntoIterator<Item = S>,
  ) -> Self {
    self
      .0
      .grants
      .entry(role.into())
      .or_default()
      .extend(permissions.into_iter().map(Into::into));
    self
  }

  /// Makes `role` inherit every permission (and parent) of `parent`.
  #[must_use]
  pub fn inherit(mut self, role: impl Into<String>, parent: impl Into<String>) -> Self {
    self
      .0
      .parents
      .entry(role.into())
      .or_default()
      .insert(parent.into());
    self
  }

  /// The table.
  pub fn build(self) -> Rbac {
    Rbac(Arc::new(self.0))
  }
}

impl Rbac {
  /// An empty table builder.
  pub fn builder() -> RbacBuilder {
    RbacBuilder::default()
  }

  /// `role` and every role it inherits from, transitively (cycles are tolerated).
  pub fn expand_role(&self, role: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![role.to_owned()];
    while let Some(next) = todo.pop() {
      if let Some(parents) = self.0.parents.get(&next) {
        todo.extend(parents.iter().filter(|p| !seen.contains(*p)).cloned());
      }
      seen.insert(next);
    }
    seen
  }

  /// Every role `principal` holds, directly or by inheritance.
  pub fn effective_roles(&self, principal: &dyn Principal) -> BTreeSet<String> {
    principal
      .roles()
      .iter()
      .flat_map(|r| self.expand_role(r))
      .collect()
  }

  /// Every permission grant of `principal`: its roles' (inherited included) and its own.
  pub fn effective_permissions(&self, principal: &dyn Principal) -> BTreeSet<String> {
    let mut perms: BTreeSet<String> = principal.permissions().iter().cloned().collect();
    for role in self.effective_roles(principal) {
      if let Some(granted) = self.0.grants.get(&role) {
        perms.extend(granted.iter().cloned());
      }
    }
    perms
  }

  /// Whether `principal` holds `role`, directly or through a role that inherits it.
  pub fn has_role(&self, principal: &dyn Principal, role: &str) -> bool {
    self.effective_roles(principal).contains(role)
  }

  /// Whether `principal` is granted `permission` (wildcards honoured).
  pub fn permits(&self, principal: &dyn Principal, permission: &str) -> bool {
    self
      .effective_permissions(principal)
      .iter()
      .any(|g| permission_matches(g, permission))
  }

  /// A policy (over any resource) allowing principals granted `permission`.
  pub fn require(&self, permission: impl Into<String>) -> RequirePermission {
    RequirePermission {
      rbac: self.clone(),
      permission: permission.into(),
    }
  }

  /// A policy (over any resource) allowing principals holding `role`, directly or inherited.
  pub fn require_role(&self, role: impl Into<String>) -> RequireInheritedRole {
    RequireInheritedRole {
      rbac: self.clone(),
      role: role.into(),
    }
  }
}

/// See [`Rbac::require`].
#[derive(Clone, Debug)]
pub struct RequirePermission {
  rbac: Rbac,
  permission: String,
}

impl<Res: ?Sized> Policy<Res> for RequirePermission {
  fn evaluate(&self, principal: &CurrentPrincipal, _: &Res) -> PolicyEffect {
    let allowed = self.rbac.permits(&**principal, &self.permission);
    succeed(Decision::allow_if(
      allowed,
      format!("missing permission {}", self.permission),
    ))
  }
}

/// See [`Rbac::require_role`].
#[derive(Clone, Debug)]
pub struct RequireInheritedRole {
  rbac: Rbac,
  role: String,
}

impl<Res: ?Sized> Policy<Res> for RequireInheritedRole {
  fn evaluate(&self, principal: &CurrentPrincipal, _: &Res) -> PolicyEffect {
    let allowed = self.rbac.has_role(&**principal, &self.role);
    succeed(Decision::allow_if(
      allowed,
      format!("requires role {}", self.role),
    ))
  }
}

/// The minimal typed environment holding only [`Rbac`].
pub type RbacEnv = HCons<Rbac, HNil>;

/// A [`Layer`] providing `rbac` as a capability.
#[inline]
pub fn provide_rbac<E: Send + 'static>(rbac: Rbac) -> Layer<RbacEnv, E> {
  Layer::succeed(hlist![rbac])
}

/// Whether the [`CurrentPrincipal`] in `R` is granted `permission` by the [`Rbac`] in `R`.
pub fn has_permission<E, R, I1, I2>(permission: impl Into<String>) -> Effect<bool, E, R>
where
  E: Send + 'static,
  R: Selector<CurrentPrincipal, I1> + Selector<Rbac, I2> + HList,
{
  let permission = permission.into();
  need::<Rbac>()
    .flat_map(move |rbac| need::<CurrentPrincipal>().map(move |p| rbac.permits(&**p, &permission)))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::principal::BasicPrincipal;
  use id_effect::Exit;

  fn rbac() -> Rbac {
    Rbac::builder()
      .grant("viewer", ["ledger:read"])
      .grant("accountant", ["ledger:post", "ledger:read"])
      .inherit("accountant", "viewer")
      .inherit("owner", "accountant")
      .grant("owner", ["ledger:*"])
      .grant("root", ["*"])
      // A cycle must not hang expansion.
      .inherit("a", "b")
      .inherit("b", "a")
      .build()
  }

  #[test]
  fn wildcards_match_by_prefix_only() {
    assert!(permission_matches("*", "anything"));
    assert!(permission_matches("ledger:read", "ledger:read"));
    assert!(permission_matches("ledger:*", "ledger:post"));
    assert!(!permission_matches("ledger:*", "ledgers:post"));
    assert!(!permission_matches("ledger*", "ledger:post"));
    assert!(!permission_matches("ledger:read", "ledger:post"));
  }

  #[test]
  fn inheritance_is_transitive_and_cycle_safe() {
    let r = rbac();
    let owner = BasicPrincipal::new("o").with_role("owner");
    assert_eq!(
      r.effective_roles(&owner),
      ["accountant", "owner", "viewer"].map(String::from).into()
    );
    assert!(r.has_role(&owner, "viewer"));
    assert!(r.permits(&owner, "ledger:close"));
    assert_eq!(r.expand_role("a"), ["a", "b"].map(String::from).into());
    assert_eq!(
      r.expand_role("unknown"),
      ["unknown"].map(String::from).into()
    );
  }

  #[test]
  fn permissions_combine_roles_and_direct_grants() {
    let r = rbac();
    let p = BasicPrincipal::new("v")
      .with_role("viewer")
      .with_permission("report:export");
    assert_eq!(
      r.effective_permissions(&p),
      ["ledger:read", "report:export"].map(String::from).into()
    );
    assert!(r.permits(&p, "report:export"));
    assert!(!r.permits(&p, "ledger:post"));
    assert!(r.permits(&BasicPrincipal::new("r").with_role("root"), "x:y"));
  }

  #[test]
  fn policies_from_the_table() {
    let r = rbac();
    let viewer = CurrentPrincipal::new(BasicPrincipal::new("v").with_role("viewer"));
    let owner = CurrentPrincipal::new(BasicPrincipal::new("o").with_role("owner"));
    let post = r.require("ledger:post");
    assert_eq!(
      post.evaluate(&viewer, &()).block_on(),
      Exit::Success(Decision::deny("missing permission ledger:post"))
    );
    assert_eq!(
      post.evaluate(&owner, &()).block_on(),
      Exit::Success(Decision::Allow)
    );
    let acct = r.require_role("accountant");
    assert_eq!(
      acct.evaluate(&owner, &()).block_on(),
      Exit::Success(Decision::Allow)
    );
    assert_eq!(
      acct.evaluate(&viewer, &()).block_on(),
      Exit::Success(Decision::deny("requires role accountant"))
    );
  }

  #[test]
  fn has_permission_reads_both_capabilities() {
    let env = || {
      hlist![
        CurrentPrincipal::new(BasicPrincipal::new("v").with_role("viewer")),
        rbac()
      ]
    };
    let read = has_permission::<(), _, _, _>("ledger:read").provide_all(env());
    assert_eq!(read.block_on(), Exit::Success(true));
    let post: Effect<bool, (), HCons<CurrentPrincipal, RbacEnv>> = has_permission("ledger:post");
    let via_layer = post
      .provide(CurrentPrincipal::new(BasicPrincipal::new("v")))
      .provide_layer(provide_rbac(rbac()));
    assert_eq!(via_layer.block_on(), Exit::Success(false));
  }
}
