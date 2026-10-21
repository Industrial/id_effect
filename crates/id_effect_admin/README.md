# id_effect_admin

Admin back office for [`id_effect`](../id_effect) services.

0.13 builds this crate in steps (ADR 0010 Amendment 9, release spec decision D2): an `AdminSite`
registration API that mounts only behind a guard, list and detail pages generated from entity field
metadata (search, filter, sort, pagination), edit forms that bind only declared fields, bulk
actions, and a hash-chained audit trail. `id-effect generate admin <Entity>` writes the registration
module. The first step is the permission model every admin page is checked against.

## Quick start

```rust
use id_effect_admin::{AdminAction, AdminPermission};

// One permission per entity and action: admin.<entity>.<list|view|create|update|delete|bulk>.
let perms = AdminPermission::all_for("document").unwrap();
assert_eq!(perms[0].as_str(), "admin.document.list");
assert!(perms.iter().filter(|p| p.action().writes()).count() == 4);

// A crafted entity name cannot widen a permission.
assert!(AdminPermission::new("*", AdminAction::Delete).is_err());
```

## Secure by default

There is no public admin: every entity × action is its own permission and a principal holds none
unless granted (ADR 0013 R1). Entity names are snake_case ASCII of at most 64 bytes.

## Examples

```bash
cargo run -p id_effect_admin --example 010_permissions
```

## Tier

`integration` (ADR 0010 Amendment 9); it moves to `platform` when the Pipeline reference app depends
on it.
