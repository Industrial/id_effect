//! Registration without a database: what `EntityAdmin` derives from the entity's field metadata,
//! slugs, prefixes and the default permission policy.

use id_effect_admin::{
  AdminAction, AdminPermission, AdminSite, EntityAdmin, Filter, RbacAdminPolicy,
};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision, Policy, Rbac};
use id_effect_sql::{Entity, FromRow};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "invoices", tenant = "org_id")]
pub struct InvoiceLine {
  #[entity(id)]
  pub id: uuid::Uuid,
  pub description: String,
  pub amount_cents: i64,
  pub paid: bool,
  pub password_hash: String,
}

#[test]
fn default_admin_shows_every_field_but_excluded() {
  let info = EntityAdmin::<InvoiceLine>::default()
    .exclude(["password_hash"])
    .info()
    .unwrap();
  let all = ["id", "description", "amount_cents", "paid"];
  assert_eq!(info.list, all);
  assert_eq!(info.detail, all);
  assert!(info.search.is_empty() && info.filters.is_empty() && info.sortable.is_empty());
  assert_eq!(info.page_size, id_effect_admin::entity::DEFAULT_PAGE_SIZE);
}

#[test]
fn slug_defaults_to_the_type_name_in_snake_case() {
  let info = EntityAdmin::<InvoiceLine>::default().info().unwrap();
  assert_eq!(
    (info.slug.as_str(), info.entity),
    ("invoice_line", "InvoiceLine")
  );
  let renamed = EntityAdmin::<InvoiceLine>::default()
    .slug("line")
    .info()
    .unwrap();
  assert_eq!(renamed.slug, "line");
  assert_eq!(renamed.permissions[1].as_str(), "admin.line.view");
}

#[test]
fn permissions_follow_action_order() {
  let info = EntityAdmin::<InvoiceLine>::default().info().unwrap();
  let actions: Vec<AdminAction> = info
    .permissions
    .iter()
    .map(AdminPermission::action)
    .collect();
  assert_eq!(actions, AdminAction::ALL);
  assert!(
    info
      .permissions
      .iter()
      .all(|p| p.entity() == "invoice_line")
  );
}

#[test]
fn typed_filters_and_sorts_are_checked_against_field_types() {
  let info = EntityAdmin::<InvoiceLine>::default()
    .search(["description"])
    .filter("paid", Filter::exact())
    .filter("amount_cents", Filter::choices(["100", "200"]))
    .sortable(["amount_cents", "id"])
    .info()
    .unwrap();
  assert_eq!(info.filters, ["paid", "amount_cents"]);
  assert_eq!(info.sortable, ["amount_cents", "id"]);
  assert_eq!(Filter::exact(), Filter::exact());
  assert_ne!(Filter::exact(), Filter::choices(["a"]));
}

#[test]
fn prefixes_may_nest() {
  let site = AdminSite::new().prefix("/ops/back-office_2");
  assert_eq!(site.path_prefix(), "/ops/back-office_2");
}

#[test]
fn a_wildcard_grant_covers_every_admin_permission() {
  let policy = RbacAdminPolicy::new(Rbac::builder().grant("root", ["*"]).build());
  let root = CurrentPrincipal::new(BasicPrincipal::new("r").with_role("root"));
  for permission in AdminPermission::all_for("invoice_line").unwrap() {
    assert_eq!(policy.decide_now(&root, &permission), Some(Decision::Allow));
  }
}
