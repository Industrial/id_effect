# The admin back office (`id_effect_admin`)

Every service with a database grows a back office: somebody has to look at the rows, find the one a
customer is asking about, and see what state it is in. Written by hand, that is a list page, a
detail page, a search box and a few filters per table, each one a chance to forget a permission
check or a tenant filter. `id_effect_admin` generates those pages from the entities you already
have:

1. **Registration**: `AdminSite::new().register::<E>(EntityAdmin::default()…)` names the columns,
   the searchable and sortable fields and the filters, checked against the entity's field metadata
   (`Entity::FIELDS`, filled by `#[derive(Entity)]`).
2. **A guard, always**: the site mounts only after `.guard(g)`. There is no public admin.
3. **A permission per entity and action**: `admin.<entity>.<list|view|create|update|delete|bulk>`,
   decided through `id_effect_authz` before any query runs.
4. **Tenant-bound reads**: every page reads through a repository bound to the request's
   `CurrentTenant`.

The pages are maud templates from `id_effect_axum`'s `ui` feature (Chapter 29.10), so every value
is escaped, and the list updates in place with htmx.

```toml
[dependencies]
id_effect_admin = "0.13"
```

## Registering entities

An entity is any `#[derive(Entity)]` struct. `EntityAdmin<E>` says how the admin shows it; every
name in it is a Rust field name of `E`:

```rust
use id_effect_admin::{AdminSite, EntityAdmin, Filter};
use id_effect_sql::repository::SortOrder;
use id_effect_sql::{Entity, FromRow};

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "documents", tenant = "org_id", soft_delete = "deleted_at")]
struct Document {
    #[entity(id)]
    id: i64,
    title: String,
    status: String,
    pages: i64,
    reviewed: bool,
    api_token: String,
}

let site = AdminSite::new().register::<Document>(
    EntityAdmin::default()
        .list(["id", "title", "status", "pages"]) // the columns of the list page
        .exclude(["api_token"])                   // never shown, on any page
        .search(["title"])                        // the search box matches these text fields
        .filter("status", Filter::choices(["draft", "published"]))
        .filter("reviewed", Filter::exact())      // a yes / no choice for a bool
        .sortable(["title", "pages"])
        .default_sort("title", SortOrder::Asc)
        .page_size(25),
);

let info = &site.entities()[0];
assert_eq!(info.slug, "document");                // the URL and permission name
assert_eq!(info.detail, ["id", "title", "status", "pages", "reviewed"]);
assert_eq!(info.permissions[0].as_str(), "admin.document.list");
```

The configuration is checked when it is registered, not when a page is first opened. A field the
entity does not have, a search over a number, a sort by a nullable column (a sort column is a
keyset cursor key and must not be NULL), a filter choice that is not a value of the field's type:
each is an `AdminConfigError`. `register` panics with it; `try_register` returns it:

```rust
use id_effect_admin::{AdminConfigError, AdminSite, EntityAdmin};
use id_effect_sql::{Entity, FromRow};

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "documents")]
struct Document {
    #[entity(id)]
    id: i64,
    title: String,
    pages: i64,
}

let refused = AdminSite::new()
    .try_register(EntityAdmin::<Document>::default().search(["pages"]))
    .unwrap_err();
assert_eq!(
    refused,
    AdminConfigError::NotSearchable { entity: "Document", field: "pages".into() }
);
```

`EntityAdmin<E>` is typed by its entity, so the configuration of one entity cannot be registered
for another, and the field metadata it is checked against is always the right one.

## The guard

`AdminSite::new()` is `AdminSite<Unguarded>`. Only `.guard(g)` turns it into an
`AdminSite<Guarded>`, and only a guarded site has `.router(db)`: an admin without a guard does not
compile (ADR 0013 R1, R7, the same typestate as `Route::public()` / `Route::guard(..)`). The guard is
any `RouteGuard`; `id_effect_authz::Guard` answers 401 to anonymous callers and 403 to principals
its policy denies. The router is an `id_effect_axum` typed router whose every route is registered
as guarded, so the application's access report lists them:

```rust
use std::sync::Arc;

use id_effect_admin::{AdminDatabase, AdminSite, EntityAdmin};
use id_effect_authz::{Guard, require_role};
use id_effect_sql::{Dialect, Entity, FromRow, TestSqlClient};

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "documents")]
struct Document {
    #[entity(id)]
    id: i64,
    title: String,
}

let db = AdminDatabase::new(Arc::new(TestSqlClient::new()), Dialect::Postgres);
let admin = AdminSite::new()
    .register::<Document>(EntityAdmin::default())
    .guard(Guard::new("staff", require_role("staff")))
    .router::<()>(db);

let report = admin.access_report();
assert!(report.public.is_empty());
let routes: Vec<&str> = report.guarded.iter().map(|(route, _)| route.as_str()).collect();
assert_eq!(routes, ["GET /admin", "GET /admin/document", "GET /admin/document/{id}"]);
```

Merge the router into the application's typed router (`Router::merge`) behind the authentication
layer that puts the principal on the request, and serve the htmx script the pages load
(`id_effect_axum::ui::htmx::asset_route()`). `.prefix("/back-office")` moves the site; `.guard(..)`
can be called again to stack guards (a second factor, an IP allowlist).

## Permissions

Inside the guard, each page is one action on one entity, and each entity × action is its own
permission:

| Page | Permission |
|---|---|
| `GET /admin` (the entities you may list) | `admin.<entity>.list`, per entry |
| `GET /admin/<entity>` (list) | `admin.<entity>.list` |
| `GET /admin/<entity>/{id}` (detail) | `admin.<entity>.view` |

The check runs before the page touches the database, and it is deny by default: a principal holds
no admin permission unless granted, and an anonymous principal is refused even behind a guard that
let it through. The default policy, `RbacAdminPolicy`, allows what an `id_effect_authz::Rbac` table
grants the principal's roles, plus the principal's own permissions. `AdminPermission::all_for`
lists an entity's six permissions for a grant:

```rust
use id_effect_admin::{AdminAction, AdminPermission, RbacAdminPolicy};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision, Policy, Rbac};

let read_only: Vec<String> = [AdminAction::List, AdminAction::View]
    .iter()
    .map(|a| AdminPermission::new("document", *a).unwrap().to_string())
    .collect();
let everything: Vec<String> = AdminPermission::all_for("document")
    .unwrap()
    .iter()
    .map(ToString::to_string)
    .collect();
let rbac = Rbac::builder()
    .grant("support", read_only)
    .grant("editor", everything)
    .build();
let policy = RbacAdminPolicy::new(rbac);

let support = CurrentPrincipal::new(BasicPrincipal::new("sam").with_role("support"));
let view = AdminPermission::new("document", AdminAction::View).unwrap();
let delete = AdminPermission::new("document", AdminAction::Delete).unwrap();
assert_eq!(policy.decide_now(&support, &view), Some(Decision::Allow));
assert!(matches!(policy.decide_now(&support, &delete), Some(Decision::Deny(_))));
```

Give the table to the site with `.rbac(rbac)`, or decide with any other
`Policy<AdminPermission>` with `.policy(p)` (one that looks the grants up in a database, say). A
principal with `list` but not `view` sees the list without links to the detail pages; a denied page
answers 403, and a policy that fails answers 500 without its error.

## Tenants

A tenant-scoped entity (`#[entity(tenant = "org_id")]`) is read through a repository bound to the
request's `CurrentTenant`: a request extension, or a capability recorded in the request's
`RequestCaps` (what the tenancy layer of Chapter 30.7 does). Search, filters, sort and the page
cursors all run inside that scope, and the soft-delete filter applies as everywhere. A record of
another tenant is "not found", exactly like a missing one. A request without a tenant is refused
for a tenant-scoped entity, never read unscoped; entities without a tenant column need none.

## The list page

The list page keeps its whole state in the query string, so every view is a link you can share:

| Parameter | Meaning |
|---|---|
| `q` | search: rows where any search field contains the text (case-insensitive) |
| `f.<field>` | a filter value: one of the declared choices, or a value of the field's type |
| `sort`, `dir` | a sortable field and `asc` / `desc` |
| `after`, `before` | the keyset cursor of the next or previous page |

Pages are keyset pages (Chapter 28.7): the cursor is opaque, tied to the search, filters and sort
it was issued for, and still filtered by the tenant, so a forged cursor only moves within the
caller's own rows. A cursor from another view answers 400. Input is bounded: a query string over
2 KiB answers 414, search text is cut to 100 characters (and `%`, `_` are matched literally), a
filter value that is not a declared choice or does not parse is ignored, and so is a sort by an
undeclared field.

The search form, the column headers and the pager are htmx links targeting the results region
(`#admin-results`, `id_effect_admin::list::RESULTS_ID`): an htmx request with that target gets only
the region, pushed into the history; any other request, a reload or a shared link, gets the whole
page in the layout.

## The detail page

`/admin/<entity>/{id}` shows the detail fields (by default every field not excluded) with their
labels. The id is parsed as the primary key's type, so `/admin/document/abc` is a 404 before any
query; text ids are percent-encoded in links.

## Labels and languages

Without a catalog, labels are the field names made readable (`created_at` → "Created at") and the
page text is English. With feature `i18n` and `.catalog(catalog)` (Chapter 30.9), the label keys are
Fluent messages in the request's locale: the entity's `#[entity(label = "..")]` (by default its
snake-case name) is a message, a field's label (`document.title`) is the attribute `title` of the
message `document`, and the page text is the message `admin` and its attributes
(`id_effect_admin::labels::UI_MESSAGES` lists them). A key no locale defines falls back to the
English default, so a partial translation never leaves a page blank:

```ftl
admin = Verwaltung
    .search = Suchen
    .next = Weiter
    .results = { $count } Treffer
document = Dokument
    .title = Titel
```

The locale is the `CurrentLocale` the `id_effect_i18n` axum layer records for the request, else a
`Locale` request extension, else the catalog's default.

## Testing an admin

Feature `test-util` adds `id_effect_admin::testing::AdminRequest`: a request carrying what the
authentication and tenancy layers would add, answered by the admin router:

```rust
use std::sync::Arc;

use axum::http::StatusCode;
use id_effect_admin::testing::AdminRequest;
use id_effect_admin::{AdminDatabase, AdminSite, EntityAdmin};
use id_effect_authz::{BasicPrincipal, Guard, require_role};
use id_effect_sql::{Dialect, Entity, FromRow, TestSqlClient};

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "documents", tenant = "org_id")]
struct Document {
    #[entity(id)]
    id: i64,
    title: String,
}

let app: axum::Router = AdminSite::new()
    .register::<Document>(EntityAdmin::default())
    .guard(Guard::new("staff", require_role("staff")))
    .router::<()>(AdminDatabase::new(Arc::new(TestSqlClient::new()), Dialect::Postgres))
    .into_axum();

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let anonymous = AdminRequest::get("/admin/document").send(&app).await;
assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

// Staff, but granted no admin permission: refused before any query.
let sam = BasicPrincipal::new("sam").with_role("staff");
let denied = AdminRequest::get("/admin/document").principal(sam).tenant(1i64).send(&app).await;
assert_eq!(denied.status, StatusCode::FORBIDDEN);

// Granted, but without a tenant: refused, never read unscoped.
let ed = BasicPrincipal::new("ed")
    .with_role("staff")
    .with_permission("admin.document.list");
let no_tenant = AdminRequest::get("/admin/document").principal(ed).send(&app).await;
assert_eq!(no_tenant.status, StatusCode::FORBIDDEN);
# });
```

`.htmx(id_effect_admin::list::RESULTS_ID)` sends the headers of the list's htmx requests, to test
the fragment the page swaps in.
