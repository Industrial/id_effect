//! The audit trail of admin changes (web.admin #6): with feature `audit`, every create, update,
//! delete, relation change and bulk action is appended to `id_effect_sql`'s hash-chained audit
//! log (`id_effect_sql::audit::AuditLog`), **in the transaction of the write it describes**: the
//! record commits or rolls back with the change.
//!
//! A record carries:
//!
//! | Audit field | Value |
//! |---|---|
//! | `actor` | the principal's id |
//! | `tenant` | the request's tenant (its own hash chain), if any |
//! | `action` | [`ACTION_CREATE`], [`ACTION_UPDATE`], [`ACTION_DELETE`], [`ACTION_RELATION_ADD`], [`ACTION_RELATION_REMOVE`], or [`ACTION_BULK_PREFIX`] and the action name |
//! | `entity` | `<entity slug>:<id>` (`<entity slug>` for a bulk action) |
//! | `before` / `after` | the changed form fields as a JSON object (an update), the form fields (a create or delete), the relation and target, or the selected ids (a bulk action) |
//!
//! Only form fields are recorded — never an excluded field or one the forms do not bind — so a
//! secret column does not leak into the log.
//!
//! Auditing is on by default with the feature: a site appends to `AuditLog::new()` (the table
//! `audit_log` of `id_effect_sql`'s audit migration) unless
//! [`AdminSite::audit_log`](crate::AdminSite::audit_log) names another log, and only the recorded
//! opt-out [`AdminSite::dangerous_without_audit`](crate::AdminSite::dangerous_without_audit)
//! turns it off. Without the feature the admin keeps no audit trail.

use std::sync::Arc;

use id_effect::{Effect, succeed};
use id_effect_sql::{SqlError, SqlTransaction, SqlValue};

#[cfg(feature = "audit")]
use crate::site::{AdminSite, SiteAccess};
use crate::site::{Ctx, SiteShared};
use crate::value::display;

/// The action of a create.
pub const ACTION_CREATE: &str = "admin.create";
/// The action of an update.
pub const ACTION_UPDATE: &str = "admin.update";
/// The action of a delete.
pub const ACTION_DELETE: &str = "admin.delete";
/// The action of a many-to-many pair added.
pub const ACTION_RELATION_ADD: &str = "admin.relation.add";
/// The action of a many-to-many pair removed.
pub const ACTION_RELATION_REMOVE: &str = "admin.relation.remove";
/// The prefix of a bulk action's audit action (`admin.bulk.delete`, `admin.bulk.<name>`).
pub const ACTION_BULK_PREFIX: &str = "admin.bulk.";

/// Who acts: the principal's id and the request's tenant.
#[derive(Clone, Debug)]
pub(crate) struct Actor {
  #[cfg_attr(not(feature = "audit"), allow(dead_code))]
  actor: String,
  #[cfg_attr(not(feature = "audit"), allow(dead_code))]
  tenant: Option<String>,
}

impl Actor {
  pub(crate) fn of(ctx: &Ctx) -> Self {
    Self {
      actor: ctx.principal_id().to_owned(),
      tenant: ctx.tenant().map(|t| display(&t.0, "true", "false")),
    }
  }
}

/// One change to record.
#[derive(Clone, Debug)]
pub(crate) struct Change {
  #[cfg_attr(not(feature = "audit"), allow(dead_code))]
  action: String,
  #[cfg_attr(not(feature = "audit"), allow(dead_code))]
  entity: String,
  before: Vec<(&'static str, SqlValue)>,
  after: Vec<(&'static str, SqlValue)>,
  #[cfg_attr(not(feature = "audit"), allow(dead_code))]
  ids: Option<Vec<String>>,
}

impl Change {
  /// `action` on the record `id` of the entity `slug`.
  pub(crate) fn new(action: &str, slug: &str, id: String) -> Self {
    Self {
      action: action.to_owned(),
      entity: format!("{slug}:{id}"),
      before: Vec::new(),
      after: Vec::new(),
      ids: None,
    }
  }

  /// The bulk action `name` on the records `ids` of the entity `slug`.
  pub(crate) fn bulk(name: &str, slug: &str, ids: Vec<String>) -> Self {
    Self {
      action: format!("{ACTION_BULK_PREFIX}{name}"),
      entity: slug.to_owned(),
      before: Vec::new(),
      after: Vec::new(),
      ids: Some(ids),
    }
  }

  /// The fields' values before.
  pub(crate) fn before(mut self, fields: Vec<(&'static str, SqlValue)>) -> Self {
    self.before = fields;
    self
  }

  /// The fields' values after.
  pub(crate) fn after(mut self, fields: Vec<(&'static str, SqlValue)>) -> Self {
    self.after = fields;
    self
  }
}

/// Append `change` by `actor` through `tx` when the site audits (feature `audit`); nothing
/// otherwise.
#[cfg_attr(not(feature = "audit"), allow(unused_variables))]
pub(crate) fn record(
  site: &SiteShared,
  actor: &Actor,
  tx: &Arc<dyn SqlTransaction>,
  change: Change,
) -> Effect<(), SqlError> {
  #[cfg(feature = "audit")]
  if let Some(log) = &site.audit {
    use id_effect_sql::audit::{AuditContext, AuditEntry};
    let context = match &actor.tenant {
      Some(tenant) => AuditContext::for_tenant(actor.actor.clone(), tenant.clone()),
      None => AuditContext::new(actor.actor.clone()),
    };
    let mut entry = AuditEntry::new(change.action, change.entity);
    if !change.before.is_empty() {
      entry = entry.with_before(object(&change.before));
    }
    if let Some(ids) = change.ids {
      entry = entry.with_after(id_effect_sql::serde_json::json!({ "ids": ids }));
    } else if !change.after.is_empty() {
      entry = entry.with_after(object(&change.after));
    }
    return log.append(tx, &context, entry).map(|_| ());
  }
  succeed(())
}

/// `fields` as a JSON object.
#[cfg(feature = "audit")]
fn object(fields: &[(&'static str, SqlValue)]) -> id_effect_sql::serde_json::Value {
  fields
    .iter()
    .map(|(name, value)| ((*name).to_owned(), json(value)))
    .collect::<id_effect_sql::serde_json::Map<_, _>>()
    .into()
}

/// A value as JSON: numbers, booleans and NULL as themselves, JSON as is, the rest as text.
#[cfg(feature = "audit")]
fn json(value: &SqlValue) -> id_effect_sql::serde_json::Value {
  use id_effect_sql::serde_json::Value;
  match value {
    SqlValue::Null => Value::Null,
    SqlValue::Bool(b) => Value::Bool(*b),
    SqlValue::I16(n) => Value::from(*n),
    SqlValue::I32(n) => Value::from(*n),
    SqlValue::I64(n) => Value::from(*n),
    SqlValue::F32(n) => Value::from(f64::from(*n)),
    SqlValue::F64(n) => Value::from(*n),
    SqlValue::Json(j) => j.clone(),
    other => Value::String(display(other, "true", "false")),
  }
}

#[cfg(feature = "audit")]
impl<A: SiteAccess> AdminSite<A> {
  /// Append the admin's changes to `log` (a table other than the default `audit_log`).
  #[must_use]
  pub fn audit_log(mut self, log: id_effect_sql::audit::AuditLog) -> Self {
    self.audit = Some(log);
    self
  }

  /// Keep no audit trail of admin changes (ADR 0013 R10). Recorded in the posture registry as
  /// `id_effect_admin::AdminSite::dangerous_without_audit`; the prod posture check reports it.
  #[must_use]
  pub fn dangerous_without_audit(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      "id_effect_admin::AdminSite::dangerous_without_audit",
      "admin creates, updates, deletes and bulk actions are not written to the audit log",
    );
    self.audit = None;
    self
  }
}

#[cfg(all(test, feature = "audit"))]
mod tests {
  use super::*;

  #[test]
  fn values_as_json() {
    use id_effect_sql::serde_json::json;
    let values = [
      ("n", SqlValue::Null),
      ("b", SqlValue::Bool(true)),
      ("s", SqlValue::I16(1)),
      ("i", SqlValue::I32(2)),
      ("l", SqlValue::I64(3)),
      ("f", SqlValue::F32(0.5)),
      ("d", SqlValue::F64(1.5)),
      ("j", SqlValue::Json(json!({"a": 1}))),
      ("t", SqlValue::Text("x".into())),
    ];
    assert_eq!(
      object(&values),
      json!({"n": null, "b": true, "s": 1, "i": 2, "l": 3, "f": 0.5, "d": 1.5, "j": {"a": 1}, "t": "x"})
    );
  }
}
