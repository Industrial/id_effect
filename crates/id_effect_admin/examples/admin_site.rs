//! A complete admin site on SQLite: documents with an author picker and labels, created, edited,
//! bulk-published and deleted through the generated pages, behind `SecurityLayer` (CSRF).
//!
//! ```bash
//! # Run the scripted session and check every step:
//! cargo run -p id_effect_admin --all-features --example admin_site -- --self-test
//! # Serve it on http://127.0.0.1:3000/admin (a fixed development principal; never in production):
//! cargo run -p id_effect_admin --all-features --example admin_site
//! ```
//!
//! The audit log's statements are PostgreSQL; on this SQLite demo the site opts out of it with
//! the recorded `dangerous_without_audit` (feature `audit`). Point `AdminDatabase` at PostgreSQL
//! with `id_effect_sql`'s audit migration applied and drop that call to audit every change.

use std::sync::Arc;

use axum::http::StatusCode;
use id_effect::{Exit, succeed};
use id_effect_admin::bulk::BulkAction;
use id_effect_admin::testing::{AdminRequest, AdminResponse};
use id_effect_admin::{
  AdminAction, AdminDatabase, AdminPermission, AdminSite, EntityAdmin, Filter,
};
use id_effect_authz::{BasicPrincipal, Guard, Rbac, require_role};
use id_effect_axum::EffectExecutor;
use id_effect_axum::server::security::{CookieKey, SecurityLayer};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_sql::{Dialect, Entity, FromRow, SqlClient};

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(
  table = "documents",
  tenant = "org_id",
  belongs_to(Person, foreign_key = "author_id", name = "author"),
  many_to_many(
    Label,
    through = "document_labels",
    local_key = "document_id",
    foreign_key = "label_id",
    name = "labels"
  )
)]
struct Document {
  #[entity(id, generated)]
  id: i64,
  #[entity(editable)]
  title: String,
  #[entity(editable)]
  status: String,
  #[entity(editable)]
  author_id: Option<i64>,
  #[entity(version)]
  version: i64,
}

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(
  table = "people",
  tenant = "org_id",
  has_many(Document, foreign_key = "author_id", name = "documents")
)]
struct Person {
  #[entity(id)]
  id: i64,
  #[entity(editable)]
  name: String,
}

#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "labels", tenant = "org_id")]
struct Label {
  #[entity(id)]
  id: i64,
  #[entity(editable)]
  name: String,
}

const SCHEMA: &[&str] = &[
  "CREATE TABLE people (id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, name TEXT NOT NULL)",
  "CREATE TABLE labels (id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, name TEXT NOT NULL)",
  "CREATE TABLE documents (id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, \
   title TEXT NOT NULL, status TEXT NOT NULL, author_id INTEGER, version INTEGER NOT NULL)",
  "CREATE TABLE document_labels (document_id INTEGER NOT NULL, label_id INTEGER NOT NULL, \
   PRIMARY KEY (document_id, label_id))",
  "INSERT INTO people (id, org_id, name) VALUES (1, 1, 'Ada'), (2, 1, 'Grace')",
  "INSERT INTO labels (id, org_id, name) VALUES (1, 1, 'urgent'), (2, 1, 'finance')",
];

async fn database() -> Arc<dyn SqlClient> {
  let pool = sqlite_pool_from_config(SqlitePoolConfig::memory().with_max_size(4))
    .await
    .expect("in-memory SQLite");
  let db: Arc<dyn SqlClient> = Arc::new(SqliteSqlClient::new(pool));
  for sql in SCHEMA {
    match EffectExecutor::global()
      .run_effect(db.execute(sql, &[]))
      .await
    {
      Exit::Success(_) => {}
      Exit::Failure(cause) => panic!("schema: {cause:?}"),
    }
  }
  db
}

/// Publish the selected drafts (a custom bulk action, through the hooks and in the audit log).
fn publish() -> BulkAction<Document> {
  BulkAction::new("publish", "Publish", |ctx, rows: Vec<Document>| {
    rows.into_iter().fold(succeed(()), |done, doc| {
      let update = ctx.update(Document {
        status: "published".into(),
        ..doc
      });
      done.flat_map(move |()| update.map(|_| ()))
    })
  })
}

fn site() -> AdminSite<id_effect_admin::Guarded> {
  let all = |entity: &str| {
    AdminPermission::all_for(entity)
      .expect("entity name")
      .iter()
      .map(ToString::to_string)
      .collect::<Vec<_>>()
  };
  let rbac = Rbac::builder()
    .grant("editor", all("document"))
    .grant("editor", all("person"))
    .grant("editor", all("label"))
    .build();
  let site = AdminSite::new()
    .title("Back office")
    .register::<Document>(
      EntityAdmin::default()
        .list(["id", "title", "status", "author_id"])
        .search(["title"])
        .filter("status", Filter::choices(["draft", "published"]))
        .sortable(["title"])
        .new_record(|| Document {
          id: 0,
          title: String::new(),
          status: "draft".into(),
          author_id: None,
          version: 0,
        })
        .action(publish().requires(AdminAction::Update)),
    )
    .register::<Person>(EntityAdmin::default().search(["name"]))
    .register::<Label>(EntityAdmin::default().search(["name"]))
    .rbac(rbac)
    .guard(Guard::new("staff", require_role("staff")));
  #[cfg(feature = "audit")]
  let site = site.dangerous_without_audit();
  site
}

async fn app() -> axum::Router {
  let key = CookieKey::generate();
  site()
    .router::<()>(AdminDatabase::new(database().await, Dialect::Sqlite))
    .into_axum()
    .layer(SecurityLayer::recommended(key))
}

fn editor() -> BasicPrincipal {
  BasicPrincipal::new("ada")
    .with_role("staff")
    .with_role("editor")
}

async fn get(app: &axum::Router, uri: &str, cookie: Option<&str>) -> AdminResponse {
  let mut req = AdminRequest::get(uri).principal(editor()).tenant(1i64);
  if let Some(cookie) = cookie {
    req = req.header(
      "cookie",
      axum::http::HeaderValue::from_str(cookie).expect("cookie"),
    );
  }
  req.send(app).await
}

async fn post(
  app: &axum::Router,
  uri: &str,
  body: &str,
  cookie: &str,
  token: &str,
) -> AdminResponse {
  AdminRequest::post(uri, body)
    .principal(editor())
    .tenant(1i64)
    .csrf(cookie, token)
    .send(app)
    .await
}

fn check(step: &str, res: &AdminResponse, status: StatusCode) {
  assert_eq!(res.status, status, "{step}: {}", res.body);
  println!("ok  {step} ({})", res.status);
}

/// The scripted session: every page and write of the admin, checked.
async fn self_test() {
  let app = app().await;
  let form = get(&app, "/admin/document/@new", None).await;
  check("create form", &form, StatusCode::OK);
  let cookie = form
    .csrf_cookie()
    .expect("the SecurityLayer issued a CSRF cookie");
  let token = form
    .csrf_token()
    .expect("the form carries the masked token");
  assert!(
    form.body.contains("Ada") && form.body.contains("Grace"),
    "author picker"
  );

  let invalid = post(
    &app,
    "/admin/document/@new",
    "title=Q3&status=draft&author_id=99",
    &cookie,
    &token,
  )
  .await;
  check(
    "create refused (no such author)",
    &invalid,
    StatusCode::UNPROCESSABLE_ENTITY,
  );
  assert!(invalid.body.contains("must reference an existing record"));

  let created = post(
    &app,
    "/admin/document/@new",
    "title=Q3+report&status=draft&author_id=1",
    &cookie,
    &token,
  )
  .await;
  check("create", &created, StatusCode::SEE_OTHER);
  assert_eq!(created.location(), Some("/admin/document/1"));

  let forged = post(
    &app,
    "/admin/document/1/@edit",
    "title=Q3+report+v2&status=draft&author_id=1&version=7&_version=0",
    &cookie,
    &token,
  )
  .await;
  check(
    "edit (the forged version is ignored)",
    &forged,
    StatusCode::SEE_OTHER,
  );

  let added = post(
    &app,
    "/admin/document/1/@rel/labels",
    "op=add&target=1",
    &cookie,
    &token,
  )
  .await;
  check("add a label", &added, StatusCode::SEE_OTHER);
  let detail = get(&app, "/admin/document/1", Some(&cookie)).await;
  check("detail", &detail, StatusCode::OK);
  assert!(detail.body.contains("Q3 report v2") && detail.body.contains("urgent"));

  let second = post(
    &app,
    "/admin/document/@new",
    "title=Budget&status=draft&author_id=2",
    &cookie,
    &token,
  )
  .await;
  check("create another", &second, StatusCode::SEE_OTHER);
  let published = post(
    &app,
    "/admin/document/@bulk",
    "action=publish&id=1&id=2",
    &cookie,
    &token,
  )
  .await;
  check("bulk publish", &published, StatusCode::SEE_OTHER);
  let list = get(&app, "/admin/document?f.status=published", Some(&cookie)).await;
  check("list of published", &list, StatusCode::OK);
  assert!(list.body.contains("2 results"), "{}", list.body);

  let no_token = AdminRequest::post("/admin/document/2/@delete", "")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  check(
    "delete without a CSRF token refused",
    &no_token,
    StatusCode::FORBIDDEN,
  );
  let confirm = get(&app, "/admin/document/2/@delete", Some(&cookie)).await;
  check("delete confirmation", &confirm, StatusCode::OK);
  let deleted = post(&app, "/admin/document/2/@delete", "", &cookie, &token).await;
  check("delete", &deleted, StatusCode::SEE_OTHER);
  let gone = get(&app, "/admin/document/2", Some(&cookie)).await;
  check("deleted record", &gone, StatusCode::NOT_FOUND);
  println!("admin_site self-test: ok");
}

#[tokio::main]
async fn main() {
  if std::env::args().any(|a| a == "--self-test") {
    self_test().await;
    return;
  }
  // Development only: every request is the editor of tenant 1 (an authentication and tenancy
  // layer puts the real principal and tenant there).
  let app = app().await.layer(axum::middleware::map_request(
    |mut req: axum::extract::Request| async move {
      req
        .extensions_mut()
        .insert(id_effect_authz::CurrentPrincipal::new(editor()));
      req
        .extensions_mut()
        .insert(id_effect_sql::CurrentTenant::new(1i64));
      req
    },
  ));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
    .await
    .expect("bind 127.0.0.1:3000");
  println!("admin on http://127.0.0.1:3000/admin (development principal; Ctrl-C to stop)");
  axum::serve(listener, app).await.expect("serve");
}
