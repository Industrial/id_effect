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
assert_eq!(routes[..3], ["GET /admin", "GET /admin/document", "GET /admin/document/{id}"]);
// …and the write pages (see "Editing records" below), every one guarded too.
assert!(routes.contains(&"POST /admin/document/{id}/@edit"));
assert_eq!(routes.len(), 12);
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

## Editing records

Every registered entity also gets a create form (`/admin/<entity>/@new`), an edit form
(`/admin/<entity>/{id}/@edit`) and a delete confirmation (`/admin/<entity>/{id}/@delete`), each
behind its own permission (`create`, `update`, `delete`). The list page shows a "New" link and the
detail page "Edit" and "Delete" links only to principals who may use them.

The forms bind **only the fields you declared editable** with `#[entity(editable)]` — never the
primary key, the version, tenant or soft-delete column. `EntityAdmin::form` narrows that set
further (it can never widen it). A submitted field the form does not declare is ignored and logged
by name: a forged `secret=…` or `org_id=…` in the body changes nothing. The other fields of a new
record come from `EntityAdmin::new_record` (or are NULL, or the column's zero for a generated id
and a version); an entity with a required field nothing fills has no create page.

```rust
use id_effect::schema::{ParseError, ParseErrors};
use id_effect::succeed;
use id_effect_admin::EntityAdmin;
use id_effect_sql::repository::EntityHooks;
use id_effect_sql::{Entity, FromRow};

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "documents", tenant = "org_id")]
struct Document {
    #[entity(id, generated)]
    id: i64,
    #[entity(editable)]
    title: String,
    #[entity(editable)]
    pages: i64,
    /// Never on a form: only the repository and the hooks write it.
    slug: String,
    #[entity(version)]
    version: i64,
}

let admin = EntityAdmin::<Document>::default()
    .form(["title", "pages"])
    .new_record(|| Document { id: 0, title: String::new(), pages: 1, slug: String::new(), version: 0 })
    .validate(|doc: &Document| {
        if doc.pages < 1 {
            return Err(ParseErrors::one(ParseError::new("pages", "must be >= 1")));
        }
        Ok(())
    })
    .hooks(EntityHooks::<Document>::new().before_insert(|_ctx, doc| {
        let slug = doc.title.to_lowercase().replace(' ', "-");
        succeed(Document { slug, ..doc })
    }));
assert_eq!(admin.info().unwrap().form, ["title", "pages"]);
```

**Validation.** Each value is parsed as its field's type, the record is decoded from the row (an
entity with `#[from_row(schema)]` runs its `Schema` and its refinements), then `validate` runs.
Every issue is bound to its field through the `ui` form helpers (Chapter 29.10) and the form comes
back with 422 and what the user typed. With feature `i18n` and a catalog built with
`.schema_messages()` (Chapter 30.9), the messages appear in the request's language — "Darf nicht
leer sein." for a German reader — while the stable message key (`schema-must-not-be-empty`) stays
in the markup.

**Writes go through the repository.** Create, update and delete run through the entity's
repository bound to the request's tenant, with the `EntityHooks` you gave `.hooks(..)` (Chapter
28.8), inside one transaction with the audit record: a failing hook rolls the write back.
Optimistic locking applies — the edit form carries the version it was rendered with, and a record
someone else changed in between answers 409 instead of being overwritten.

**Secure by default** (ADR 0013): a form body over 64 KiB answers 413 before it is decoded
(`id_effect_admin::forms::MAX_FORM_BYTES`; `forms::decode_form` is the bounded decoder, fuzzed as
`admin_form_body`), and every `POST` must have passed the masked-token CSRF check of
`id_effect_axum`'s `SecurityLayer` (Chapter 30). Install the layer around the application; the
forms render the token. A write the layer did not check is refused with 403, whatever its body says.

## Relations

The admin reads the relations you declared on the entity (Chapter 28.6) and shows each one whose
target is registered on the same site and listable by the principal:

| Relation | On the admin |
|---|---|
| `belongs_to` | the foreign key is a picker on the forms: a select of the target's rows and a search box that refreshes it (`/admin/<entity>/@pick/<relation>?_q=…`, htmx) |
| `has_many` | an inline list of the children on the detail page, linking to their pages |
| `many_to_many` | an inline list with a "Remove" button per row and an "Add" picker, writing the join table |

Everything a relation shows is read through the target's repository bound to the request's
tenant: a picker never offers another tenant's rows, and a foreign key or an "Add" naming one is
refused ("must reference an existing record" on the form, 400 for the join table). Adding a pair
twice writes it once.

## Bulk actions

The list page shows a checkbox per row and an action menu to principals holding
`admin.<entity>.bulk` and the action's own permission: the built-in `delete` needs
`admin.<entity>.delete`, a custom action the one it `requires` (`update` by default).

```rust
use id_effect::succeed;
use id_effect_admin::bulk::BulkAction;
use id_effect_admin::{AdminAction, EntityAdmin};
use id_effect_sql::{Entity, FromRow};

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "documents")]
struct Document {
    #[entity(id)]
    id: i64,
    #[entity(editable)]
    status: String,
}

let publish = BulkAction::new("publish", "Publish", |ctx, rows: Vec<Document>| {
    rows.into_iter().fold(succeed(()), |done, doc| {
        // Through the entity's hooks, in the action's transaction.
        let update = ctx.update(Document { status: "published".into(), ..doc });
        done.flat_map(move |()| update.map(|_| ()))
    })
})
.requires(AdminAction::Update);
let admin = EntityAdmin::<Document>::default().action(publish);
assert_eq!(admin.info().unwrap().actions, ["publish"]);
```

A request selects at most 100 records (`id_effect_admin::bulk::MAX_BULK_IDS`); more is refused
with 400 before any query. The selected ids are read through the tenant-bound repository, so an id
of another tenant selects nothing, and the action runs in one transaction with its audit record.

## The audit trail

With feature `audit`, every create, update, delete, join-table change and bulk action is appended
to `id_effect_sql`'s hash-chained audit log (Chapter 30.7) **in the transaction of the write**, so
the record commits or rolls back with the change. A record names the principal (`actor`), the
tenant (its own chain), the action (`admin.create`, `admin.update`, `admin.delete`,
`admin.relation.add`, `admin.relation.remove`, `admin.bulk.<name>`), the record
(`<entity>:<id>`) and the changed fields as JSON — an update records only what changed, and only
form fields ever reach the log, so an excluded secret column never does.

Auditing is on by default with the feature, into the default `audit_log` table
(`.audit_log(AuditLog::new().with_table(..))` picks another). Turning it off takes
`.dangerous_without_audit()`, which is recorded in the posture registry and reported by the prod
posture check.

The crate's example runs a whole session — create with a picker, a refused foreign key, an edit
with a forged field, a label added, a bulk publish, a refused delete without a CSRF token, a
confirmed delete:

```bash
cargo run -p id_effect_admin --all-features --example admin_site -- --self-test
```
