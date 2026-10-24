# id_effect_admin

Admin back office for [`id_effect`](../id_effect) services.

Register entities on an `AdminSite`, put it behind a guard, and get list and detail pages generated
from the entities' field metadata (`#[derive(Entity)]`): text search over declared columns, typed
filters, sortable columns and keyset pages, rendered with maud and updated in place by htmx (the
`id_effect_axum` `ui` feature), with labels translated through `id_effect_i18n` (feature `i18n`).
Edit forms, bulk actions, relation pickers, the audit trail and `id-effect generate admin
<Entity>` follow in 0.13 (ADR 0010 Amendment 9, release spec decision D2).

## Quick start

```rust
use std::sync::Arc;

use id_effect_admin::{AdminAction, AdminDatabase, AdminPermission, AdminSite, EntityAdmin, Filter};
use id_effect_authz::{Guard, Rbac, require_role};
use id_effect_sql::{Dialect, Entity, FromRow, TestSqlClient};

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "documents", tenant = "org_id")]
struct Document {
  #[entity(id)]
  id: i64,
  title: String,
  status: String,
}

// One permission per entity and action: admin.<entity>.<list|view|create|update|delete|bulk>.
let read: Vec<String> = [AdminAction::List, AdminAction::View]
  .iter()
  .map(|a| AdminPermission::new("document", *a).unwrap().to_string())
  .collect();

let admin = AdminSite::new()
  .register::<Document>(
    EntityAdmin::default()
      .search(["title"])
      .filter("status", Filter::choices(["draft", "published"]))
      .sortable(["title"]),
  )
  .rbac(Rbac::builder().grant("support", read).build())
  .guard(Guard::new("staff", require_role("staff"))) // no guard, no router
  .router::<()>(AdminDatabase::new(Arc::new(TestSqlClient::new()), Dialect::Postgres));

assert!(admin.access_report().public.is_empty());

// A crafted entity name cannot widen a permission.
assert!(AdminPermission::new("*", AdminAction::Delete).is_err());
```

## Secure by default

- No public admin: an `AdminSite` mounts only after `.guard(g)` (an unguarded site has no router),
  and every route is registered as guarded.
- Every entity × action is its own permission, checked through `id_effect_authz` before any query;
  a principal holds none unless granted, and anonymous principals are always refused.
- Pages read through a repository bound to the request's `CurrentTenant`; a tenant-scoped entity
  without a tenant is refused.
- maud escapes every value; the crate never builds unescaped markup. Query strings, search text,
  filters and page cursors are bounded.

## Examples

```bash
cargo run -p id_effect_admin --example 010_permissions
```

Book: *The admin back office* (`part6/ch30-08-admin.md`).

## Tier

`integration` (ADR 0010 Amendment 9); it moves to `platform` when the Pipeline reference app depends
on it.
