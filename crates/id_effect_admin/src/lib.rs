//! Admin back office for id_effect services.
//!
//! 0.13 builds this crate in steps (ADR 0010 Amendment 9, release spec 0.13.0 decision D2): an
//! `AdminSite` registration API (`AdminSite::register::<E>()`) that mounts only behind a guard,
//! list and detail pages generated from entity field metadata, edit forms, bulk actions and an
//! audit trail. This first release step provides the permission model every admin page is checked
//! against.
//!
//! ## Secure by default
//!
//! Every entity × action pair is its own permission, `admin.<entity>.<action>`
//! ([`AdminPermission`]), and a principal holds none of them unless granted (ADR 0013 R1). Entity
//! names are validated, so a permission string can never be widened by a crafted name such as
//! `*` or `a.b`.
//!
//! ```
//! use id_effect_admin::{AdminAction, AdminPermission};
//!
//! let perm = AdminPermission::new("document", AdminAction::Update).unwrap();
//! assert_eq!(perm.as_str(), "admin.document.update");
//! assert!(AdminPermission::new("*", AdminAction::Delete).is_err());
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

/// Compiles and runs the README quick start as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

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
    if entity.is_empty() {
      return Err(AdminPermissionError::EmptyEntity);
    }
    if entity.len() > Self::MAX_ENTITY_LEN {
      return Err(AdminPermissionError::EntityTooLong);
    }
    let starts_with_letter = entity.as_bytes()[0].is_ascii_lowercase();
    let snake = entity
      .bytes()
      .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !starts_with_letter || !snake {
      return Err(AdminPermissionError::InvalidEntity);
    }
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

impl fmt::Display for AdminPermission {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.name)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

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
}
