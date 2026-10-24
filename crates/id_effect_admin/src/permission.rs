//! The admin permission model: one permission per entity × action, `admin.<entity>.<action>`,
//! checked through `id_effect_authz` before any query runs (ADR 0013 R1, web.admin #3).
//!
//! A principal holds none of them unless granted. [`RbacAdminPolicy`], the default policy of an
//! [`AdminSite`](crate::AdminSite), allows a permission when the principal is granted it by an
//! `id_effect_authz::Rbac` table (through one of its roles, or directly as a principal
//! permission); any other `Policy<AdminPermission>` can replace it
//! ([`AdminSite::policy`](crate::AdminSite::policy)).
//!
//! ```
//! use id_effect_admin::{AdminAction, AdminPermission, RbacAdminPolicy};
//! use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision, Policy, Rbac};
//!
//! let rbac = Rbac::builder()
//!   .grant("editor", ["admin.document.list", "admin.document.view"])
//!   .build();
//! let policy = RbacAdminPolicy::new(rbac);
//! let editor = CurrentPrincipal::new(BasicPrincipal::new("ed").with_role("editor"));
//! let list = AdminPermission::new("document", AdminAction::List).unwrap();
//! let delete = AdminPermission::new("document", AdminAction::Delete).unwrap();
//! assert_eq!(policy.decide_now(&editor, &list), Some(Decision::Allow));
//! assert!(matches!(policy.decide_now(&editor, &delete), Some(Decision::Deny(_))));
//! ```

use std::fmt;

use id_effect::{Cause, Exit, hlist, succeed};
use id_effect_authz::{
  AuthzError, BoxPolicy, CurrentPrincipal, Decision, Policy, PolicyEffect, Rbac, TenantScope,
  authorize_with,
};
use id_effect_axum::EffectExecutor;

/// An action the admin can perform on a registered entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AdminAction {
  /// See the list page (search, filter, sort, paginate).
  List,
  /// See one record's detail page.
  View,
  /// Create a record.
  Create,
  /// Change a record.
  Update,
  /// Delete a record.
  Delete,
  /// Run a bulk action over selected records.
  Bulk,
}

impl AdminAction {
  /// Every action, in the order the admin shows them.
  pub const ALL: [Self; 6] = [
    Self::List,
    Self::View,
    Self::Create,
    Self::Update,
    Self::Delete,
    Self::Bulk,
  ];

  /// The action's name in a permission string (`list`, `view`, …).
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::List => "list",
      Self::View => "view",
      Self::Create => "create",
      Self::Update => "update",
      Self::Delete => "delete",
      Self::Bulk => "bulk",
    }
  }

  /// Whether the action changes data (create, update, delete, bulk).
  pub const fn writes(self) -> bool {
    !matches!(self, Self::List | Self::View)
  }
}

impl fmt::Display for AdminAction {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The permission `admin.<entity>.<action>` that guards one action on one entity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AdminPermission {
  name: String,
  entity_len: usize,
  action: AdminAction,
}

/// Why [`AdminPermission::new`] rejected an entity name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminPermissionError {
  /// The entity name was empty.
  EmptyEntity,
  /// The entity name was longer than [`AdminPermission::MAX_ENTITY_LEN`] bytes.
  EntityTooLong,
  /// The entity name has a character outside `a-z`, `0-9` and `_`, or does not start with a
  /// letter.
  InvalidEntity,
}

impl fmt::Display for AdminPermissionError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::EmptyEntity => "admin entity name is empty",
      Self::EntityTooLong => "admin entity name is longer than the limit",
      Self::InvalidEntity => "admin entity name must be snake_case ASCII starting with a letter",
    })
  }
}

impl std::error::Error for AdminPermissionError {}

impl AdminPermission {
  /// The longest entity name accepted, in bytes.
  pub const MAX_ENTITY_LEN: usize = 64;

  const PREFIX: &'static str = "admin.";

  /// The permission for `action` on `entity` (a snake_case entity name such as `document`).
  ///
  /// # Errors
  ///
  /// Returns an [`AdminPermissionError`] when `entity` is empty, longer than
  /// [`AdminPermission::MAX_ENTITY_LEN`] bytes, or not snake_case ASCII starting with a letter.
  pub fn new(entity: &str, action: AdminAction) -> Result<Self, AdminPermissionError> {
    check_entity(entity)?;
    Ok(Self {
      name: format!("{}{entity}.{}", Self::PREFIX, action.as_str()),
      entity_len: entity.len(),
      action,
    })
  }

  /// Every permission for `entity`, one per [`AdminAction`].
  ///
  /// # Errors
  ///
  /// As [`AdminPermission::new`].
  pub fn all_for(entity: &str) -> Result<Vec<Self>, AdminPermissionError> {
    AdminAction::ALL
      .iter()
      .map(|action| Self::new(entity, *action))
      .collect()
  }

  /// The permission string, for example `admin.document.update`.
  pub fn as_str(&self) -> &str {
    &self.name
  }

  /// The entity name.
  pub fn entity(&self) -> &str {
    &self.name[Self::PREFIX.len()..Self::PREFIX.len() + self.entity_len]
  }

  /// The action.
  pub fn action(&self) -> AdminAction {
    self.action
  }
}

/// `entity` is a valid entity name: snake_case ASCII starting with a letter, at most
/// [`AdminPermission::MAX_ENTITY_LEN`] bytes.
pub(crate) fn check_entity(entity: &str) -> Result<(), AdminPermissionError> {
  let Some(first) = entity.bytes().next() else {
    return Err(AdminPermissionError::EmptyEntity);
  };
  if entity.len() > AdminPermission::MAX_ENTITY_LEN {
    return Err(AdminPermissionError::EntityTooLong);
  }
  let snake = entity
    .bytes()
    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
  if !first.is_ascii_lowercase() || !snake {
    return Err(AdminPermissionError::InvalidEntity);
  }
  Ok(())
}

impl fmt::Display for AdminPermission {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.name)
  }
}

/// A permission belongs to no tenant: the tenant of the rows is enforced by the tenant-bound
/// repository, not by the permission check.
impl TenantScope for AdminPermission {}

/// The default admin policy: allow an [`AdminPermission`] when the (authenticated) principal is
/// granted its string by an [`Rbac`] table — through a role, or as one of the principal's own
/// permissions. With the empty table ([`Default`]) only principal permissions count.
///
/// `Rbac` wildcards work as everywhere in `id_effect_authz`: `*` grants every admin permission;
/// `prefix:*` patterns do not match the dotted `admin.<entity>.<action>` names, so grant the
/// entity's permissions explicitly ([`AdminPermission::all_for`]).
#[derive(Clone, Debug, Default)]
pub struct RbacAdminPolicy {
  rbac: Rbac,
}

impl RbacAdminPolicy {
  /// The policy granting what `rbac` grants.
  pub fn new(rbac: Rbac) -> Self {
    Self { rbac }
  }

  fn decide(&self, principal: &CurrentPrincipal, permission: &AdminPermission) -> Decision {
    let granted =
      principal.is_authenticated() && self.rbac.permits(&**principal, permission.as_str());
    Decision::allow_if(granted, format!("missing permission {permission}"))
  }
}

impl Policy<AdminPermission> for RbacAdminPolicy {
  fn evaluate(&self, principal: &CurrentPrincipal, permission: &AdminPermission) -> PolicyEffect {
    succeed(self.decide(principal, permission))
  }

  fn decide_now(
    &self,
    principal: &CurrentPrincipal,
    permission: &AdminPermission,
  ) -> Option<Decision> {
    Some(self.decide(principal, permission))
  }
}

/// Decide `permission` for `principal` with `policy`: `Ok(())` when allowed, else the
/// [`AuthzError`] (`Unauthenticated` for the anonymous principal, whatever the policy; `Denied`
/// when the policy denies; `Evaluation` when it failed). A pure policy decides without a fiber.
pub(crate) async fn authorize(
  policy: &BoxPolicy<AdminPermission>,
  principal: &CurrentPrincipal,
  permission: AdminPermission,
) -> Result<(), AuthzError> {
  if !principal.is_authenticated() {
    return Err(AuthzError::Unauthenticated);
  }
  if let Some(decision) = policy.decide_now(principal, &permission) {
    return match decision {
      Decision::Allow => Ok(()),
      Decision::Deny(denial) => Err(AuthzError::Denied(denial)),
    };
  }
  let check =
    authorize_with(BoxPolicy::clone(policy), permission).provide_all(hlist![principal.clone()]);
  match EffectExecutor::global().run_effect(check).await {
    Exit::Success(()) => Ok(()),
    Exit::Failure(Cause::Fail(error)) => Err(error),
    Exit::Failure(other) => Err(AuthzError::Evaluation(format!("{other:?}"))),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_authz::BasicPrincipal;

  #[test]
  fn admin_permission_names_entity_and_action() {
    let perm = AdminPermission::new("document_label", AdminAction::Bulk).unwrap();
    assert_eq!(perm.as_str(), "admin.document_label.bulk");
    assert_eq!(perm.to_string(), "admin.document_label.bulk");
    assert_eq!(perm.entity(), "document_label");
    assert_eq!(perm.action(), AdminAction::Bulk);
  }

  #[test]
  fn admin_actions_cover_read_and_write() {
    let names: Vec<String> = AdminAction::ALL.iter().map(ToString::to_string).collect();
    assert_eq!(
      names,
      ["list", "view", "create", "update", "delete", "bulk"]
    );
    let writes: Vec<bool> = AdminAction::ALL.iter().map(|a| a.writes()).collect();
    assert_eq!(writes, [false, false, true, true, true, true]);
  }

  #[test]
  fn admin_permission_rejects_wildcards_and_oversize_names() {
    use AdminPermissionError::*;
    assert_eq!(
      AdminPermission::new("", AdminAction::List),
      Err(EmptyEntity)
    );
    let long = "a".repeat(AdminPermission::MAX_ENTITY_LEN + 1);
    assert_eq!(
      AdminPermission::new(&long, AdminAction::List),
      Err(EntityTooLong)
    );
    for bad in ["*", "a.b", "Document", "1doc", "_doc", "doc-x", "dóc"] {
      assert_eq!(
        AdminPermission::new(bad, AdminAction::List),
        Err(InvalidEntity),
        "{bad}"
      );
    }
    assert_eq!(AdminPermission::all_for("a*"), Err(InvalidEntity));
    for err in [EmptyEntity, EntityTooLong, InvalidEntity] {
      assert!(!err.to_string().is_empty());
    }
  }

  #[test]
  fn rbac_admin_policy_denies_by_default_and_anonymous() {
    let perm = AdminPermission::new("document", AdminAction::View).unwrap();
    let nobody = CurrentPrincipal::new(BasicPrincipal::new("nobody"));
    let empty = RbacAdminPolicy::default();
    assert!(matches!(
      empty.decide_now(&nobody, &perm),
      Some(Decision::Deny(_))
    ));
    let direct =
      CurrentPrincipal::new(BasicPrincipal::new("d").with_permission("admin.document.view"));
    assert_eq!(empty.decide_now(&direct, &perm), Some(Decision::Allow));
    let all = RbacAdminPolicy::new(Rbac::builder().grant("root", ["*"]).build());
    let anonymous = CurrentPrincipal::anonymous();
    assert!(matches!(
      all.decide_now(&anonymous, &perm),
      Some(Decision::Deny(_))
    ));
    assert_eq!(all.decide_now(&direct, &perm), Some(Decision::Allow));
    let exit = empty.evaluate(&nobody, &perm).block_on();
    assert!(matches!(exit, Exit::Success(Decision::Deny(_))));
  }
}
