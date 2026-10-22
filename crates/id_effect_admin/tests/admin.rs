//! The admin site end to end on SQLite in memory (feature `test-util`): registration, the guard
//! typestate, per-entity × action permissions, tenant-bound list and detail pages, search,
//! filters, sort and keyset pages, htmx fragments, escaping and labels.
//!
//! ADR 0013: `admin_denied_without_permission`, `admin_action_requires_entity_permission`,
//! `admin_list_scoped_to_tenant` (R1), `no_preescaped_outside_trusted_module` (R9).

use std::sync::Arc;

use axum::http::StatusCode;
use axum::http::request::Parts;
use id_effect::{Exit, fail, succeed};
use id_effect_admin::list::{MAX_QUERY_LEN, RESULTS_ID};
use id_effect_admin::testing::AdminRequest;
use id_effect_admin::{
  AdminAction, AdminConfigError, AdminDatabase, AdminPermission, AdminSite, EntityAdmin, Filter,
  Guarded,
};
use id_effect_authz::{
  AuthzError, BasicPrincipal, CurrentPrincipal, Decision, Guard, Policy, PolicyEffect, Rbac,
  require_role,
};
use id_effect_axum::route::{GuardFuture, RouteGuard};
use id_effect_axum::ui::{LayoutContext, Markup, html};
use id_effect_axum::{EffectExecutor, RequestCaps};
use id_effect_sql::repository::SortOrder;
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_sql::{CurrentTenant, Dialect, Entity, FromRow, SqlClient, SqlValue};

// ---- the model ----

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "adm_documents", tenant = "org_id", soft_delete = "deleted_at")]
pub struct Document {
  #[entity(id)]
  pub id: i64,
  pub title: String,
  pub status: String,
  pub pages: i64,
  pub published: bool,
  pub note: Option<String>,
  pub secret: String,
  pub due: jiff::civil::Date,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "adm_labels", label = "tag")]
pub struct Label {
  #[entity(id)]
  pub code: String,
  pub name: String,
}

/// A measurement: a float key and a JSON payload (neither readable from a request).
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "adm_measures")]
pub struct Measure {
  #[entity(id)]
  pub id: f64,
  pub data: serde_json::Value,
}

/// A hand-written entity without field metadata.
#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct Plain {
  pub id: i64,
}

impl Entity for Plain {
  type Id = i64;
  const NAME: &'static str = "Plain";
  const TABLE: &'static str = "plain";
  const COLUMNS: &'static [&'static str] = &["id"];
  const ID: &'static str = "id";

  fn id(&self) -> i64 {
    self.id
  }

  fn values(&self) -> Vec<(&'static str, SqlValue)> {
    vec![("id", SqlValue::I64(self.id))]
  }
}

// ---- the database ----

async fn run<A: Send + 'static>(eff: id_effect::Effect<A, id_effect_sql::SqlError>) -> A {
  match EffectExecutor::global().run_effect(eff).await {
    Exit::Success(a) => a,
    Exit::Failure(cause) => panic!("{cause:?}"),
  }
}

async fn empty_db() -> Arc<dyn SqlClient> {
  let pool = sqlite_pool_from_config(SqlitePoolConfig::memory().with_max_size(4))
    .await
    .expect("sqlite pool");
  Arc::new(SqliteSqlClient::new(pool))
}

async fn exec(db: &Arc<dyn SqlClient>, sql: &str) {
  run(db.execute(sql, &[])).await;
}

/// Org 1: documents 1..=25 (`doc 01` … `doc 25`; odd ones `draft`, even ones `published`), plus
/// a soft-deleted one. Org 2: two documents of its own. Two labels.
async fn seeded_db() -> Arc<dyn SqlClient> {
  let db = empty_db().await;
  exec(
    &db,
    "CREATE TABLE adm_documents (id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, \
     title TEXT NOT NULL, status TEXT NOT NULL, pages INTEGER NOT NULL, \
     published BOOLEAN NOT NULL, note TEXT, secret TEXT NOT NULL, due DATE NOT NULL, \
     deleted_at TEXT)",
  )
  .await;
  exec(
    &db,
    "CREATE TABLE adm_labels (code TEXT PRIMARY KEY, name TEXT NOT NULL)",
  )
  .await;
  for id in 1..=25 {
    let status = if id % 2 == 1 { "draft" } else { "published" };
    let published = i32::from(id % 2 == 0);
    let note = if id == 3 {
      "'third'".to_owned()
    } else {
      "NULL".to_owned()
    };
    exec(
      &db,
      &format!(
        "INSERT INTO adm_documents (id, org_id, title, status, pages, published, note, secret, \
         due) VALUES ({id}, 1, 'doc {id:02}', '{status}', {}, {published}, {note}, 'hunter2', \
         '2026-10-{:02}')",
        100 - id,
        id
      ),
    )
    .await;
  }
  exec(
    &db,
    "INSERT INTO adm_documents (id, org_id, title, status, pages, published, secret, due, \
     deleted_at) VALUES (26, 1, 'doc gone', 'draft', 1, 0, 's', '2026-10-01', \
     '2026-10-02T00:00:00Z')",
  )
  .await;
  exec(
    &db,
    "INSERT INTO adm_documents (id, org_id, title, status, pages, published, secret, due) VALUES \
     (101, 2, 'Globex secret plan', 'draft', 7, 0, 's', '2026-10-01'), \
     (102, 2, 'Globex alpha memo', 'published', 9, 1, 's', '2026-10-01')",
  )
  .await;
  exec(
    &db,
    "INSERT INTO adm_labels (code, name) VALUES ('a b', 'Spaced'), ('urgent', 'Urgent')",
  )
  .await;
  db
}

// ---- the site ----

fn documents() -> EntityAdmin<Document> {
  EntityAdmin::default()
    .list(["id", "title", "status", "pages", "published", "note"])
    .exclude(["secret"])
    .search(["title", "status"])
    .filter("status", Filter::choices(["draft", "published"]))
    .filter("published", Filter::exact())
    .filter("pages", Filter::exact())
    .sortable(["title", "pages"])
    .page_size(10)
}

fn perms(entity: &str, actions: &[AdminAction]) -> Vec<String> {
  actions
    .iter()
    .map(|a| AdminPermission::new(entity, *a).unwrap().to_string())
    .collect()
}

fn rbac() -> Rbac {
  use AdminAction::*;
  Rbac::builder()
    .grant("editor", perms("document", &[List, View]))
    .grant("editor", perms("label", &[List, View]))
    .grant("lister", perms("document", &[List]))
    .build()
}

fn site() -> AdminSite<Guarded> {
  AdminSite::new()
    .register::<Document>(documents())
    .register::<Label>(EntityAdmin::default())
    .rbac(rbac())
    .guard(Guard::new("staff", require_role("staff")))
}

fn app(db: Arc<dyn SqlClient>) -> axum::Router {
  site()
    .router::<()>(AdminDatabase::new(db, Dialect::Sqlite))
    .into_axum()
}

fn editor() -> BasicPrincipal {
  BasicPrincipal::new("ed").with_roles(["staff", "editor"])
}

fn lister() -> BasicPrincipal {
  BasicPrincipal::new("li").with_roles(["staff", "lister"])
}

/// The ids linked from a list page, in order.
fn linked_ids(body: &str, entity: &str) -> Vec<String> {
  let marker = format!("<a href=\"/admin/{entity}/");
  body
    .match_indices(&marker)
    .map(|(at, _)| {
      let rest = &body[at + marker.len()..];
      rest[..rest.find('"').unwrap()].to_owned()
    })
    .collect()
}

/// The titles on a list page, in order.
fn titles(body: &str) -> Vec<String> {
  body
    .match_indices("<td>doc ")
    .map(|(at, _)| body[at + 4..at + 10].to_owned())
    .collect()
}

/// The `href` of the link with `rel`.
fn rel_link(body: &str, rel: &str) -> String {
  let marker = format!("rel=\"{rel}\" href=\"");
  let at = body
    .find(&marker)
    .unwrap_or_else(|| panic!("no {rel} link in {body}"))
    + marker.len();
  body[at..at + body[at..].find('"').unwrap()].replace("&amp;", "&")
}

// ---- registration ----

#[test]
fn admin_registration_is_typed() {
  let site = site();
  let infos = site.entities();
  assert_eq!(infos.len(), 2);
  let doc = &infos[0];
  assert_eq!(
    (doc.slug.as_str(), doc.entity, doc.table),
    ("document", "Document", "adm_documents")
  );
  assert_eq!(doc.label, "document");
  assert_eq!(
    doc.list,
    ["id", "title", "status", "pages", "published", "note"]
  );
  assert_eq!(
    doc.detail,
    ["id", "title", "status", "pages", "published", "note", "due"]
  );
  assert_eq!(doc.search, ["title", "status"]);
  assert_eq!(doc.filters, ["status", "published", "pages"]);
  assert_eq!(doc.sortable, ["title", "pages"]);
  assert_eq!(doc.page_size, 10);
  let names: Vec<String> = doc.permissions.iter().map(ToString::to_string).collect();
  assert_eq!(
    names,
    AdminPermission::all_for("document")
      .unwrap()
      .iter()
      .map(ToString::to_string)
      .collect::<Vec<_>>()
  );
  assert_eq!((infos[1].slug.as_str(), infos[1].label), ("label", "tag"));
  assert_eq!(site.path_prefix(), "/admin");
  assert_eq!(site.guards().policies().collect::<Vec<_>>(), ["staff"]);

  // Every mounted route is guarded; none is public.
  let router = site.router::<()>(AdminDatabase::new(
    Arc::new(id_effect_sql::TestSqlClient::new()),
    Dialect::Sqlite,
  ));
  let report = router.access_report();
  assert!(report.public.is_empty() && !report.has_opt_outs());
  let guarded: Vec<&str> = report.guarded.iter().map(|(r, _)| r.as_str()).collect();
  let mut expected = vec!["GET /admin".to_owned()];
  for slug in ["document", "label"] {
    // Read pages, then the write pages of r13-e9-3-admin-edit.
    for route in [
      "GET /admin/{}",
      "GET /admin/{}/{id}",
      "GET /admin/{}/@new",
      "POST /admin/{}/@new",
      "GET /admin/{}/{id}/@edit",
      "POST /admin/{}/{id}/@edit",
      "GET /admin/{}/{id}/@delete",
      "POST /admin/{}/{id}/@delete",
      "POST /admin/{}/@bulk",
      "GET /admin/{}/@pick/{relation}",
      "POST /admin/{}/{id}/@rel/{relation}",
    ] {
      expected.push(route.replacen("{}", slug, 1));
    }
  }
  assert_eq!(guarded, expected);
  assert!(report.guarded.iter().all(|(_, p)| p == &["staff"]));
}

#[test]
fn registration_errors_name_the_problem() {
  use AdminConfigError::*;
  let err = |r: Result<AdminSite, AdminConfigError>| r.expect_err("refused");
  let fresh = AdminSite::new;
  let cases = [
    (
      err(fresh().try_register(EntityAdmin::<Document>::default().list(["titel"]))),
      UnknownField {
        entity: "Document",
        field: "titel".into(),
      },
    ),
    (
      err(fresh().try_register(EntityAdmin::<Document>::default().exclude(["nope"]))),
      UnknownField {
        entity: "Document",
        field: "nope".into(),
      },
    ),
    (
      err(
        fresh().try_register(
          EntityAdmin::<Document>::default()
            .exclude(["secret"])
            .detail(["secret"]),
        ),
      ),
      Excluded {
        entity: "Document",
        field: "secret".into(),
      },
    ),
    (
      err(fresh().try_register(EntityAdmin::<Document>::default().search(["pages"]))),
      NotSearchable {
        entity: "Document",
        field: "pages".into(),
      },
    ),
    (
      err(fresh().try_register(EntityAdmin::<Document>::default().sortable(["note"]))),
      NotSortable {
        entity: "Document",
        field: "note".into(),
      },
    ),
    (
      err(
        fresh()
          .try_register(EntityAdmin::<Document>::default().default_sort("note", SortOrder::Asc)),
      ),
      NotSortable {
        entity: "Document",
        field: "note".into(),
      },
    ),
    (
      err(fresh().try_register(
        EntityAdmin::<Document>::default().filter("pages", Filter::choices(["many"])),
      )),
      InvalidChoice {
        entity: "Document",
        field: "pages".into(),
        choice: "many".into(),
      },
    ),
    (
      err(fresh().try_register(EntityAdmin::<Document>::default().page_size(0))),
      PageSize {
        entity: "Document",
        size: 0,
      },
    ),
    (
      err(fresh().try_register(EntityAdmin::<Document>::default().page_size(501))),
      PageSize {
        entity: "Document",
        size: 501,
      },
    ),
    (
      err(fresh().try_register(EntityAdmin::<Plain>::default())),
      NoFieldMetadata { entity: "Plain" },
    ),
    (
      err(fresh().try_register(EntityAdmin::<Measure>::default())),
      UnsupportedId { entity: "Measure" },
    ),
    (
      err(
        fresh()
          .register::<Document>(EntityAdmin::default())
          .try_register(EntityAdmin::<Label>::default().slug("document")),
      ),
      DuplicateSlug {
        slug: "document".into(),
      },
    ),
  ];
  for (got, want) in cases {
    assert_eq!(got, want);
    assert!(got.to_string().starts_with("admin"), "{got}");
  }
  let bad_slug = err(fresh().try_register(EntityAdmin::<Label>::default().slug("Tags!")));
  assert!(matches!(bad_slug, InvalidSlug { ref slug, .. } if slug == "Tags!"));
  assert!(bad_slug.to_string().contains("Tags!"));
  let measure = EntityAdmin::<Measure>::default().slug("measure");
  assert!(matches!(
    err(fresh().try_register(measure.clone().filter("data", Filter::exact()))),
    UnsupportedId { .. }
  ));
  let prefix = InvalidPrefix {
    prefix: "admin/".into(),
  };
  assert!(prefix.to_string().contains("admin/"));
  for error in [
    NotFilterable {
      entity: "Measure",
      field: "data".into(),
    },
    UnsupportedId { entity: "Measure" },
  ] {
    assert!(!error.to_string().is_empty());
  }
  let info = EntityAdmin::<Label>::default()
    .list(["name"])
    .detail(["name"])
    .info()
    .unwrap();
  assert_eq!((info.list, info.detail), (vec!["name"], vec!["name"]));
  assert!(format!("{:?}", EntityAdmin::<Label>::default()).contains("EntityAdmin"));
}

#[test]
fn json_fields_cannot_be_filtered() {
  #[derive(Clone, Debug, PartialEq, FromRow, Entity)]
  #[entity(table = "adm_events")]
  pub struct Event {
    #[entity(id)]
    pub id: i64,
    pub data: serde_json::Value,
  }
  let refused = AdminSite::new()
    .try_register(EntityAdmin::<Event>::default().filter("data", Filter::exact()))
    .err();
  assert_eq!(
    refused,
    Some(AdminConfigError::NotFilterable {
      entity: "Event",
      field: "data".into()
    })
  );
  let refused = AdminSite::new()
    .try_register(EntityAdmin::<Event>::default().sortable(["data"]))
    .err();
  assert!(matches!(
    refused,
    Some(AdminConfigError::NotSortable { .. })
  ));
}

#[test]
#[should_panic(expected = "is not a field")]
fn register_panics_on_a_refused_configuration() {
  let _ = AdminSite::new().register::<Document>(EntityAdmin::default().search(["missing"]));
}

#[test]
#[should_panic(expected = "path prefix")]
fn prefix_must_be_path_segments() {
  let _ = AdminSite::new().prefix("admin/");
}

#[test]
fn guards_stack_and_prefix_moves_routes() {
  let site = AdminSite::default()
    .prefix("/back-office/admin")
    .register::<Label>(EntityAdmin::default())
    .guard(Guard::new("staff", require_role("staff")))
    .guard(Guard::new("mfa", require_role("mfa")));
  assert_eq!(
    site.guards().policies().collect::<Vec<_>>(),
    ["staff", "mfa"]
  );
  assert_eq!(format!("{:?}", site.guards()), r#"["staff", "mfa"]"#);
  assert!(format!("{site:?}").contains("back-office"));
  let router = site.router::<()>(AdminDatabase::new(
    Arc::new(id_effect_sql::TestSqlClient::new()),
    Dialect::Sqlite,
  ));
  let report = router.access_report();
  assert_eq!(report.guarded[0].0, "GET /back-office/admin");
  assert_eq!(report.guarded[0].1, ["staff", "mfa"]);
  let db = AdminDatabase::new(
    Arc::new(id_effect_sql::TestSqlClient::new()),
    Dialect::Postgres,
  );
  assert_eq!(db.dialect(), Dialect::Postgres);
  assert!(format!("{db:?}").contains("Postgres"));
  assert!(format!("{:?}", AdminSite::new()).contains("Unguarded"));
}

// ---- permissions (ADR 0013 R1) ----

#[tokio::test]
async fn admin_denied_without_permission() {
  let app = app(seeded_db().await);
  // The site guard: anonymous callers and principals outside it never reach a page.
  let anonymous = AdminRequest::get("/admin/document")
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
  let outsider = AdminRequest::get("/admin/document")
    .principal(BasicPrincipal::new("eve").with_role("editor"))
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(outsider.status, StatusCode::FORBIDDEN);
  // Inside the guard, but granted nothing: every page is refused, nothing is listed.
  let staff = || BasicPrincipal::new("sam").with_role("staff");
  for uri in [
    "/admin/document",
    "/admin/document/1",
    "/admin/label",
    "/admin/label/urgent",
  ] {
    let res = AdminRequest::get(uri)
      .principal(staff())
      .tenant(1i64)
      .send(&app)
      .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{uri}");
    assert!(res.body.contains("You may not do this."), "{uri}");
    assert!(!res.body.contains("doc 01"), "{uri}");
  }
  let index = AdminRequest::get("/admin")
    .principal(staff())
    .send(&app)
    .await;
  assert_eq!(index.status, StatusCode::OK);
  assert!(index.body.contains("Nothing to administer."));
  assert!(!index.body.contains("/admin/document"));
}

#[tokio::test]
async fn admin_action_requires_entity_permission() {
  let app = app(seeded_db().await);
  // `admin.document.list` only: the list, without links to the detail pages.
  let list = AdminRequest::get("/admin/document")
    .principal(lister())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(list.status, StatusCode::OK);
  assert!(list.body.contains("<td>doc 01</td>"));
  assert!(linked_ids(&list.body, "document").is_empty());
  let detail = AdminRequest::get("/admin/document/1")
    .principal(lister())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(detail.status, StatusCode::FORBIDDEN);
  let labels = AdminRequest::get("/admin/label")
    .principal(lister())
    .send(&app)
    .await;
  assert_eq!(labels.status, StatusCode::FORBIDDEN);
  let index = AdminRequest::get("/admin")
    .principal(lister())
    .send(&app)
    .await;
  assert!(
    index
      .body
      .contains(r#"<a href="/admin/document">Document</a>"#)
  );
  assert!(!index.body.contains("/admin/label"));
  // With `view` the rows link to their detail pages.
  let list = AdminRequest::get("/admin/document")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(linked_ids(&list.body, "document").len(), 10);
}

/// Allows `list` only, deciding inside an effect (no `decide_now`).
struct OnlyList;

impl Policy<AdminPermission> for OnlyList {
  fn evaluate(&self, _: &CurrentPrincipal, permission: &AdminPermission) -> PolicyEffect {
    succeed(Decision::allow_if(
      permission.action() == AdminAction::List,
      "only list",
    ))
  }
}

/// A policy whose backing store is down.
struct Broken;

impl Policy<AdminPermission> for Broken {
  fn evaluate(&self, _: &CurrentPrincipal, _: &AdminPermission) -> PolicyEffect {
    fail(AuthzError::Evaluation("policy store down".into()))
  }
}

/// A guard admitting everyone, anonymous callers included.
struct Open;

impl RouteGuard for Open {
  fn policy(&self) -> &str {
    "open"
  }

  fn check<'a>(&'a self, _: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async { Ok(()) })
  }
}

#[tokio::test]
async fn custom_policies_decide_every_action() {
  let db = seeded_db().await;
  let with = |site: AdminSite<Guarded>| {
    site
      .router::<()>(AdminDatabase::new(Arc::clone(&db), Dialect::Sqlite))
      .into_axum()
  };
  let only_list = with(
    AdminSite::new()
      .register::<Document>(documents())
      .policy(OnlyList)
      .guard(Open),
  );
  let list = AdminRequest::get("/admin/document")
    .principal(lister())
    .tenant(1i64)
    .send(&only_list)
    .await;
  assert_eq!(list.status, StatusCode::OK);
  let detail = AdminRequest::get("/admin/document/1")
    .principal(lister())
    .tenant(1i64)
    .send(&only_list)
    .await;
  assert_eq!(detail.status, StatusCode::FORBIDDEN);
  // An anonymous caller past an open guard is still not an admin (401).
  let anonymous = AdminRequest::get("/admin/document")
    .tenant(1i64)
    .send(&only_list)
    .await;
  assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
  let rbac_anonymous = with(
    AdminSite::new()
      .register::<Document>(documents())
      .guard(Open),
  );
  let res = AdminRequest::get("/admin/document")
    .tenant(1i64)
    .send(&rbac_anonymous)
    .await;
  assert_eq!(res.status, StatusCode::UNAUTHORIZED);
  // A policy that cannot decide fails closed.
  let broken = with(
    AdminSite::new()
      .register::<Document>(documents())
      .policy(Broken)
      .guard(Open),
  );
  let res = AdminRequest::get("/admin/document")
    .principal(editor())
    .tenant(1i64)
    .send(&broken)
    .await;
  assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
  assert!(res.body.contains("Something went wrong."));
  assert!(!res.body.contains("policy store down"));
}

// ---- tenancy (ADR 0013 R1) ----

#[tokio::test]
async fn admin_list_scoped_to_tenant() {
  let app = app(seeded_db().await);
  let acme = AdminRequest::get("/admin/document?q=alpha")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(acme.status, StatusCode::OK);
  assert!(acme.body.contains("0 results"), "{}", acme.body);
  assert!(!acme.body.contains("Globex"));
  let all = AdminRequest::get("/admin/document")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert!(
    all.body.contains("25 results"),
    "the soft-deleted row and org 2 are hidden"
  );
  // Org 2 sees only its own rows (the tenant from the request capabilities this time).
  let mut caps = RequestCaps::new();
  caps.insert(CurrentTenant::new(2i64));
  let globex = AdminRequest::get("/admin/document")
    .principal(editor())
    .extension(caps)
    .send(&app)
    .await;
  assert!(globex.body.contains("2 results"));
  assert!(globex.body.contains("Globex secret plan"));
  assert!(!globex.body.contains("doc 01"));
  // Another tenant's record is not found, the same as a missing one.
  let crossing = AdminRequest::get("/admin/document/101")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(crossing.status, StatusCode::NOT_FOUND);
  assert!(!crossing.body.contains("Globex"));
  // No tenant: a tenant-scoped entity is refused, never read unscoped.
  for uri in ["/admin/document", "/admin/document/1"] {
    let res = AdminRequest::get(uri).principal(editor()).send(&app).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{uri}");
    assert!(!res.body.contains("Globex") && !res.body.contains("doc 01"));
  }
  // An entity of no tenant needs none.
  let labels = AdminRequest::get("/admin/label")
    .principal(editor())
    .send(&app)
    .await;
  assert_eq!(labels.status, StatusCode::OK);
  assert!(labels.body.contains("2 results"));
}

// ---- list: search, filters, sort, pages ----

#[tokio::test]
async fn admin_list_search_filter_sort_page() {
  let app = app(seeded_db().await);
  let get = |uri: &str| {
    AdminRequest::get(uri.to_owned())
      .principal(editor())
      .tenant(1i64)
      .send(&app)
  };

  // The full page: layout, heading, search form, filters, sortable headers, first page.
  let page = get("/admin/document").await;
  assert_eq!(page.status, StatusCode::OK);
  assert!(page.body.starts_with("<!DOCTYPE html>"));
  assert!(
    page
      .body
      .contains("<title>Document · Administration</title>")
  );
  assert!(page.body.contains(r#"<input type="search" name="q""#));
  assert!(page.body.contains(
    r#"<select name="f.status"><option value="">Any</option><option value="draft">draft</option>"#
  ));
  assert!(page.body.contains(r#"<option value="true">Yes</option>"#));
  assert!(page.body.contains(r#"<input type="text" name="f.pages">"#));
  assert!(
    page
      .body
      .contains(r#"href="/admin/document?sort=title&amp;dir=asc""#)
  );
  assert!(page.body.contains("25 results"));
  assert_eq!(
    linked_ids(&page.body, "document"),
    ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]
  );
  assert!(page.body.contains("<td>Yes</td>") && page.body.contains("<td>No</td>"));
  assert!(
    !page.body.contains("hunter2"),
    "excluded fields are never shown"
  );
  assert!(!page.body.contains("rel=\"prev\""));

  // Search: case-insensitive, over the declared columns; LIKE wildcards are literal.
  let found = get("/admin/document?q=DOC%201").await;
  assert!(found.body.contains("10 results"), "doc 10 … doc 19");
  let by_status = get("/admin/document?q=publ").await;
  assert!(by_status.body.contains("12 results"));
  let wildcard = get("/admin/document?q=%25").await;
  assert!(wildcard.body.contains("0 results") && wildcard.body.contains("No records match."));
  assert!(wildcard.body.contains(r#"value="%""#));

  // Filters: a declared choice, a typed boolean, a typed number; bad values are ignored.
  let drafts = get("/admin/document?f.status=draft").await;
  assert!(drafts.body.contains("13 results"));
  assert!(
    drafts
      .body
      .contains(r#"<option value="draft" selected>draft</option>"#)
  );
  let bogus = get("/admin/document?f.status=archived&f.pages=many").await;
  assert!(bogus.body.contains("25 results"));
  let published = get("/admin/document?f.published=true&f.pages=90").await;
  assert!(published.body.contains("1 results"));
  assert_eq!(linked_ids(&published.body, "document"), ["10"]);
  let both = get("/admin/document?f.status=draft&f.published=false&q=doc%202").await;
  assert_eq!(linked_ids(&both.body, "document"), ["21", "23", "25"]);

  // Sort: declared fields only, either way; the header links flip the order.
  let desc = get("/admin/document?sort=title&dir=desc").await;
  assert_eq!(titles(&desc.body)[..3], ["doc 25", "doc 24", "doc 23"]);
  assert!(desc.body.contains(r#"aria-sort="descending""#));
  assert!(
    desc
      .body
      .contains(r#"href="/admin/document?sort=title&amp;dir=asc""#)
  );
  assert!(
    desc
      .body
      .contains(r#"<input type="hidden" name="dir" value="desc">"#)
  );
  let by_pages = get("/admin/document?sort=pages").await;
  assert_eq!(
    linked_ids(&by_pages.body, "document")[0],
    "25",
    "fewest pages first"
  );
  assert!(
    by_pages
      .body
      .contains(r#"href="/admin/document?sort=pages&amp;dir=desc""#)
  );
  let unsortable = get("/admin/document?sort=secret&dir=desc").await;
  assert_eq!(linked_ids(&unsortable.body, "document")[0], "1");

  // Keyset pages: next, next, then back.
  let next = rel_link(&page.body, "next");
  assert!(next.starts_with("/admin/document?after="));
  let second = get(&next).await;
  assert_eq!(linked_ids(&second.body, "document")[0], "11");
  assert!(second.body.contains("rel=\"prev\""));
  let third = get(&rel_link(&second.body, "next")).await;
  assert_eq!(
    linked_ids(&third.body, "document"),
    ["21", "22", "23", "24", "25"]
  );
  assert!(!third.body.contains("rel=\"next\""));
  let back = get(&rel_link(&third.body, "prev")).await;
  assert_eq!(linked_ids(&back.body, "document")[0], "11");
  // A cursor keeps the view it was issued for.
  let sorted = get("/admin/document?sort=title&dir=desc").await;
  let sorted_next = rel_link(&sorted.body, "next");
  assert!(sorted_next.contains("sort=title&dir=desc&after="));
  assert_eq!(titles(&get(&sorted_next).await.body)[0], "doc 15");

  // Refused: a cursor of another view, a token that is not a cursor, both directions, too long.
  let token = next.split("after=").nth(1).unwrap();
  let other_view = get(&format!("/admin/document?f.status=draft&after={token}")).await;
  assert_eq!(other_view.status, StatusCode::BAD_REQUEST);
  assert!(other_view.body.contains("This request is not valid."));
  assert_eq!(
    get("/admin/document?after=not-a-cursor").await.status,
    StatusCode::BAD_REQUEST
  );
  assert_eq!(
    get(&format!("/admin/document?after={token}&before={token}"))
      .await
      .status,
    StatusCode::BAD_REQUEST
  );
  let long = format!("/admin/document?q={}", "x".repeat(MAX_QUERY_LEN));
  assert_eq!(get(&long).await.status, StatusCode::URI_TOO_LONG);
}

#[tokio::test]
async fn htmx_gets_the_results_region() {
  let app = app(seeded_db().await);
  let results = AdminRequest::get("/admin/document?q=doc%201")
    .principal(editor())
    .tenant(1i64)
    .htmx(RESULTS_ID)
    .send(&app)
    .await;
  assert_eq!(results.status, StatusCode::OK);
  assert!(
    results.body.starts_with(&format!(
      "<div class=\"admin-results\" id=\"{RESULTS_ID}\">"
    )),
    "{}",
    results.body
  );
  assert!(!results.body.contains("<h1>") && !results.body.contains("<form"));
  assert_eq!(results.headers["vary"], "hx-request");
  // Any other htmx request gets the page content without the layout.
  let fragment = AdminRequest::get("/admin/document")
    .principal(editor())
    .tenant(1i64)
    .htmx("main")
    .send(&app)
    .await;
  assert!(fragment.body.starts_with("<section class=\"admin-list\">"));
  assert!(fragment.body.contains("<form") && !fragment.body.contains("<!DOCTYPE"));
  // The page wires its form, headers and pager to the results region.
  let page = AdminRequest::get("/admin/document")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert!(page.body.contains(&format!(
    r##"hx-target="#{RESULTS_ID}" hx-push-url="true""##
  )));
}

// ---- detail ----

#[tokio::test]
async fn admin_detail_renders_fields() {
  let app = app(seeded_db().await);
  let get = |uri: &'static str| {
    AdminRequest::get(uri)
      .principal(editor())
      .tenant(1i64)
      .send(&app)
  };
  let doc = get("/admin/document/3").await;
  assert_eq!(doc.status, StatusCode::OK);
  assert!(doc.body.contains("<h1>Document 3</h1>"));
  assert!(
    doc
      .body
      .contains("<title>Document 3 · Administration</title>")
  );
  for (label, value) in [
    ("Id", "3"),
    ("Title", "doc 03"),
    ("Status", "draft"),
    ("Pages", "97"),
    ("Published", "No"),
    ("Note", "third"),
    ("Due", "2026-10-03"),
  ] {
    assert!(
      doc
        .body
        .contains(&format!("<dt>{label}</dt><dd>{value}</dd>")),
      "{label}: {}",
      doc.body
    );
  }
  assert!(!doc.body.contains("Secret") && !doc.body.contains("hunter2"));
  assert!(
    doc
      .body
      .contains(r#"<a class="admin-back" href="/admin/document">Back to the list</a>"#)
  );
  assert!(
    doc
      .body
      .contains(r#"<a href="/admin/document">Document</a> › <span aria-current="page">3</span>"#)
  );
  let empty_note = get("/admin/document/4").await;
  assert!(empty_note.body.contains("<dt>Note</dt><dd></dd>"));
  // Text ids are percent-encoded in links and decoded from the path.
  let labels = get("/admin/label").await;
  assert!(labels.body.contains(r#"href="/admin/label/a%20b""#));
  let spaced = get("/admin/label/a%20b").await;
  assert_eq!(spaced.status, StatusCode::OK);
  assert!(spaced.body.contains("<dt>Name</dt><dd>Spaced</dd>"));
  assert!(
    spaced
      .body
      .contains("<title>Label a b · Administration</title>")
  );
  // Missing, soft-deleted and unparsable ids are all not found.
  for uri in [
    "/admin/document/999",
    "/admin/document/26",
    "/admin/document/abc",
  ] {
    let res = get(uri).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{uri}");
    assert!(res.body.contains("This record does not exist."));
  }
}

#[tokio::test]
async fn database_errors_answer_500_without_details() {
  // The tables do not exist.
  let app = app(empty_db().await);
  for uri in ["/admin/document", "/admin/document/1"] {
    let res = AdminRequest::get(uri)
      .principal(editor())
      .tenant(1i64)
      .send(&app)
      .await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR, "{uri}");
    assert!(res.body.contains("Something went wrong."));
    assert!(!res.body.contains("adm_documents"));
  }
}

// ---- output (ADR 0013 R9) ----

#[tokio::test]
async fn admin_escapes_record_values() {
  let db = seeded_db().await;
  exec(
    &db,
    "INSERT INTO adm_documents (id, org_id, title, status, pages, published, secret, due) VALUES \
     (50, 1, '<script>alert(1)</script>', 'draft', 1, 0, 's', '2026-10-01')",
  )
  .await;
  let app = app(db);
  for uri in ["/admin/document?q=script", "/admin/document/50"] {
    let res = AdminRequest::get(uri)
      .principal(editor())
      .tenant(1i64)
      .send(&app)
      .await;
    assert!(
      res.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
      "{uri}"
    );
    assert!(!res.body.contains("<script>alert"), "{uri}");
  }
}

#[test]
fn no_preescaped_outside_trusted_module() {
  // Raw HTML enters pages only through `id_effect_axum::ui::trusted_html`; this crate never names
  // `PreEscaped` (the views leaf's test covers `crates/id_effect_axum/src`).
  let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
  let mut stack = vec![src];
  let mut checked = 0;
  while let Some(dir) = stack.pop() {
    for entry in std::fs::read_dir(&dir).unwrap() {
      let path = entry.unwrap().path();
      if path.is_dir() {
        stack.push(path);
      } else if path.extension().is_some_and(|e| e == "rs") {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
          !text.contains("PreEscaped"),
          "{} names PreEscaped",
          path.display()
        );
        checked += 1;
      }
    }
  }
  assert!(checked >= 10, "the admin sources were found ({checked})");
}

// ---- layout and labels ----

#[tokio::test]
async fn site_title_stylesheets_and_layout() {
  let db = seeded_db().await;
  let styled = AdminSite::new()
    .title("Back office")
    .stylesheet("/admin.css")
    .register::<Label>(EntityAdmin::default())
    .rbac(rbac())
    .guard(Guard::new("staff", require_role("staff")))
    .router::<()>(AdminDatabase::new(Arc::clone(&db), Dialect::Sqlite))
    .into_axum();
  let res = AdminRequest::get("/admin/label")
    .principal(editor())
    .send(&styled)
    .await;
  assert!(
    res
      .body
      .contains(r#"<link rel="stylesheet" href="/admin.css">"#)
  );
  assert!(res.body.contains("<title>Label · Back office</title>"));
  assert!(res.body.contains(r#"<a href="/admin">Back office</a>"#));

  let layout = |_: &LayoutContext, title: &str, content: Markup| html! { main data-title=(title) { (content) } };
  let custom = AdminSite::new()
    .register::<Label>(EntityAdmin::default().list(["name"]))
    .rbac(rbac())
    .layout(layout)
    .guard(Guard::new("staff", require_role("staff")))
    .router::<()>(AdminDatabase::new(db, Dialect::Sqlite))
    .into_axum();
  let res = AdminRequest::get("/admin/label")
    .principal(editor())
    .send(&custom)
    .await;
  assert!(res.body.starts_with("<main data-title=\""), "{}", res.body);
  assert!(
    res
      .body
      .contains("<td><a href=\"/admin/label/a%20b\">Spaced</a></td>")
  );
}

#[cfg(feature = "i18n")]
#[tokio::test]
async fn labels_follow_the_request_locale() {
  use id_effect_i18n::{Catalog, CurrentLocale, Locale};

  let en = Locale::parse("en").unwrap();
  let de = Locale::parse("de").unwrap();
  let catalog = Catalog::builder(en.clone())
    .embedded(en.clone(), "admin.ftl", "admin = Administration\n")
    .embedded(
      de.clone(),
      "admin.ftl",
      "admin = Verwaltung\n  .next = Weiter\n  .results = { $count } Treffer\n  .yes = Ja\n\
       document = Dokument\n  .title = Titel\n",
    )
    .use_isolating(false)
    .build()
    .unwrap();
  let app = AdminSite::new()
    .register::<Document>(documents())
    .rbac(rbac())
    .catalog(catalog)
    .guard(Guard::new("staff", require_role("staff")))
    .router::<()>(AdminDatabase::new(seeded_db().await, Dialect::Sqlite))
    .into_axum();
  let german = AdminRequest::get("/admin/document")
    .principal(editor())
    .tenant(1i64)
    .extension(de.clone())
    .send(&app)
    .await;
  let body = &german.body;
  assert!(body.contains(r#"<html lang="de">"#));
  assert!(body.contains("<title>Dokument · Verwaltung</title>"));
  assert!(body.contains("25 Treffer") && body.contains(">Titel<") && body.contains(">Weiter<"));
  assert!(body.contains("<td>Ja</td>"));
  assert!(
    body.contains(">Status<"),
    "a missing key falls back to the field name"
  );
  // The locale an i18n layer records in the request capabilities wins.
  let mut caps = RequestCaps::new();
  caps.insert(CurrentLocale::new(de));
  caps.insert(CurrentTenant::new(1i64));
  let res = AdminRequest::get("/admin/document")
    .principal(editor())
    .extension(caps)
    .send(&app)
    .await;
  assert!(res.body.contains("25 Treffer"));
  // Without a locale: the catalog's default.
  let english = AdminRequest::get("/admin/document")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert!(english.body.contains(r#"<html lang="en">"#) && english.body.contains("25 results"));
  assert!(english.body.contains("· Administration</title>"));
}

#[tokio::test]
async fn test_requests_carry_headers() {
  let app = app(seeded_db().await);
  let req =
    AdminRequest::get("/admin").header("x-trace", axum::http::HeaderValue::from_static("1"));
  assert!(format!("{req:?}").contains("x-trace"));
  let res = req.principal(editor()).send(&app).await;
  assert_eq!(res.status, StatusCode::OK);
  assert!(format!("{res:?}").contains("200"));
}
