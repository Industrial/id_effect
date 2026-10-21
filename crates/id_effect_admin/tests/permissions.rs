//! Public-API test of the admin permission model (r13-t0-skeletons).

use id_effect_admin::{AdminAction, AdminPermission};

#[test]
fn admin_permissions_one_per_action_and_distinct() {
  let perms = AdminPermission::all_for("source").unwrap();
  let names: Vec<&str> = perms.iter().map(AdminPermission::as_str).collect();
  assert_eq!(
    names,
    [
      "admin.source.list",
      "admin.source.view",
      "admin.source.create",
      "admin.source.update",
      "admin.source.delete",
      "admin.source.bulk",
    ]
  );
  assert!(perms.iter().all(|p| p.entity() == "source"));
  let actions: Vec<AdminAction> = perms.iter().map(AdminPermission::action).collect();
  assert_eq!(actions, AdminAction::ALL);
}
