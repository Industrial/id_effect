//! Resource policies: ownership ([`Owned`], [`owner`]), tenant match ([`Tenanted`],
//! [`same_tenant`]) and access-control lists ([`Acl`], [`HasAcl`], [`acl_permits`]).
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision, Owned, Policy, owner};
//!
//! struct Draft {
//!   author: String,
//! }
//!
//! impl Owned for Draft {
//!   fn owner_id(&self) -> &str {
//!     &self.author
//!   }
//! }
//!
//! let draft = Draft { author: "alice".into() };
//! let alice = CurrentPrincipal::new(BasicPrincipal::new("alice"));
//! let bob = CurrentPrincipal::new(BasicPrincipal::new("bob"));
//! assert_eq!(owner().evaluate(&alice, &draft).block_on(), Exit::Success(Decision::Allow));
//! assert!(matches!(owner().evaluate(&bob, &draft).block_on(), Exit::Success(Decision::Deny(_))));
//! ```

use id_effect::succeed;

use crate::policy::{Decision, Policy, PolicyEffect};
use crate::principal::{CurrentPrincipal, Principal};
use crate::rbac::permission_matches;

/// A resource with an owning principal.
pub trait Owned {
  /// The [`Principal::id`] of the owner.
  fn owner_id(&self) -> &str;
}

/// A resource that belongs to a tenant.
pub trait Tenanted {
  /// The tenant id, compared with [`Principal::tenant`].
  fn tenant_id(&self) -> &str;
}

/// Allows the principal that owns the resource.
pub fn owner() -> OwnerPolicy {
  OwnerPolicy
}

/// See [`owner`].
#[derive(Clone, Copy, Debug)]
pub struct OwnerPolicy;

impl<Res: Owned + ?Sized> Policy<Res> for OwnerPolicy {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    let owns = principal.is_authenticated() && principal.id() == resource.owner_id();
    succeed(Decision::allow_if(owns, "not the owner"))
  }
}

/// Allows a principal whose tenant is the resource's tenant (no tenant never matches).
pub fn same_tenant() -> SameTenant {
  SameTenant
}

/// See [`same_tenant`].
#[derive(Clone, Copy, Debug)]
pub struct SameTenant;

impl<Res: Tenanted + ?Sized> Policy<Res> for SameTenant {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    let same = principal.tenant() == Some(resource.tenant_id());
    succeed(Decision::allow_if(
      same,
      "resource belongs to another tenant",
    ))
  }
}

/// Who an [`Acl`] entry grants to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Grantee {
  /// One principal, by [`Principal::id`].
  User(String),
  /// Every principal holding this role directly.
  Role(String),
  /// Every authenticated principal.
  Authenticated,
  /// Everyone, the anonymous principal included.
  Anyone,
}

impl Grantee {
  /// Whether `principal` is covered by this grantee.
  pub fn covers(&self, principal: &dyn Principal) -> bool {
    match self {
      Self::User(id) => principal.is_authenticated() && principal.id() == id,
      Self::Role(role) => principal.has_role(role),
      Self::Authenticated => principal.is_authenticated(),
      Self::Anyone => true,
    }
  }
}

/// A per-resource access-control list: (grantee, permission) entries; permissions accept the
/// same `area:*` / `*` wildcards as [`Rbac`](crate::Rbac).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Acl {
  entries: Vec<(Grantee, String)>,
}

impl Acl {
  /// An empty list (denies everything).
  pub fn new() -> Self {
    Self::default()
  }

  /// Grants `permission` to `grantee`.
  #[must_use]
  pub fn grant(mut self, grantee: Grantee, permission: impl Into<String>) -> Self {
    self.entries.push((grantee, permission.into()));
    self
  }

  /// Grants `permission` to the principal with id `id`.
  #[must_use]
  pub fn allow_user(self, id: impl Into<String>, permission: impl Into<String>) -> Self {
    self.grant(Grantee::User(id.into()), permission)
  }

  /// Grants `permission` to holders of `role`.
  #[must_use]
  pub fn allow_role(self, role: impl Into<String>, permission: impl Into<String>) -> Self {
    self.grant(Grantee::Role(role.into()), permission)
  }

  /// The entries, in insertion order.
  pub fn entries(&self) -> &[(Grantee, String)] {
    &self.entries
  }

  /// Whether some entry grants `permission` to `principal`.
  pub fn permits(&self, principal: &dyn Principal, permission: &str) -> bool {
    self
      .entries
      .iter()
      .any(|(who, granted)| who.covers(principal) && permission_matches(granted, permission))
  }
}

/// A resource carrying its own [`Acl`].
pub trait HasAcl {
  /// The resource's access-control list.
  fn acl(&self) -> &Acl;
}

impl HasAcl for Acl {
  fn acl(&self) -> &Acl {
    self
  }
}

/// Allows principals the resource's [`Acl`] grants `permission` to.
pub fn acl_permits(permission: impl Into<String>) -> AclPolicy {
  AclPolicy(permission.into())
}

/// See [`acl_permits`].
#[derive(Clone, Debug)]
pub struct AclPolicy(String);

impl<Res: HasAcl + ?Sized> Policy<Res> for AclPolicy {
  fn evaluate(&self, principal: &CurrentPrincipal, resource: &Res) -> PolicyEffect {
    let allowed = resource.acl().permits(&**principal, &self.0);
    succeed(Decision::allow_if(
      allowed,
      format!("no access-list entry grants {}", self.0),
    ))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::principal::{Anonymous, BasicPrincipal};
  use id_effect::Exit;

  struct Account {
    owner: String,
    org: String,
  }

  impl Owned for Account {
    fn owner_id(&self) -> &str {
      &self.owner
    }
  }

  impl Tenanted for Account {
    fn tenant_id(&self) -> &str {
      &self.org
    }
  }

  fn decide<Res, P: Policy<Res>>(policy: &P, who: impl Principal, res: &Res) -> Decision {
    match policy.evaluate(&CurrentPrincipal::new(who), res).block_on() {
      Exit::Success(d) => d,
      other => panic!("unexpected exit: {other:?}"),
    }
  }

  fn account() -> Account {
    Account {
      owner: "alice".into(),
      org: "acme".into(),
    }
  }

  #[test]
  fn ownership() {
    assert!(decide(&owner(), BasicPrincipal::new("alice"), &account()).is_allowed());
    assert_eq!(
      decide(&owner(), BasicPrincipal::new("bob"), &account()),
      Decision::deny("not the owner")
    );
    let unowned = Account {
      owner: String::new(),
      org: "acme".into(),
    };
    assert!(decide(&owner(), Anonymous, &unowned).is_denied());
  }

  #[test]
  fn tenant_match() {
    let acme = BasicPrincipal::new("bob").with_tenant("acme");
    assert!(decide(&same_tenant(), acme, &account()).is_allowed());
    let other = BasicPrincipal::new("bob").with_tenant("globex");
    assert!(decide(&same_tenant(), other, &account()).is_denied());
    assert!(decide(&same_tenant(), BasicPrincipal::new("bob"), &account()).is_denied());
  }

  #[test]
  fn grantees_cover_the_right_principals() {
    let alice = BasicPrincipal::new("alice").with_role("auditor");
    assert!(Grantee::User("alice".into()).covers(&alice));
    assert!(!Grantee::User(String::new()).covers(&Anonymous));
    assert!(Grantee::Role("auditor".into()).covers(&alice));
    assert!(Grantee::Authenticated.covers(&alice));
    assert!(!Grantee::Authenticated.covers(&Anonymous));
    assert!(Grantee::Anyone.covers(&Anonymous));
  }

  #[test]
  fn acl_entries_and_policy() {
    let acl = Acl::new()
      .allow_user("bob", "account:read")
      .allow_role("auditor", "account:*")
      .grant(Grantee::Anyone, "account:list");
    assert_eq!(acl.entries().len(), 3);
    let read = acl_permits("account:read");
    assert!(decide(&read, BasicPrincipal::new("bob"), &acl).is_allowed());
    assert!(decide(&read, BasicPrincipal::new("eve").with_role("auditor"), &acl).is_allowed());
    assert_eq!(
      decide(&read, BasicPrincipal::new("eve"), &acl),
      Decision::deny("no access-list entry grants account:read")
    );
    assert!(decide(&acl_permits("account:list"), Anonymous, &acl).is_allowed());
    assert!(!Acl::new().permits(&BasicPrincipal::new("bob"), "x"));
  }
}
