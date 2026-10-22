//! Admin editing end to end on SQLite behind `SecurityLayer`: create, edit and delete forms,
//! validation errors bound to fields, the mass-assignment guard, the form body cap, CSRF,
//! lifecycle hooks, optimistic locking and the audit trail.
//!
//! ADR 0013: `admin_form_ignores_undeclared_fields` (R1, R4), `r4_admin_form_over_cap_413`,
//! `r9_admin_post_without_csrf_token_refused`, `admin_change_written_to_audit_log` (R10).

mod support;

use axum::http::StatusCode;
use id_effect::schema::{ParseError, ParseErrors};
use id_effect::{fail, succeed};
use id_effect_admin::forms::{MAX_FORM_BYTES, decode_form};
use id_effect_admin::testing::AdminRequest;
use id_effect_admin::{AdminConfigError, AdminSite, EntityAdmin, Guarded};
use id_effect_sql::SqlError;
use id_effect_sql::repository::EntityHooks;
use support::{
  Article, Author, Db, Note, Session, Tag, app, article, bare_app, editor, form, guarded, seeded,
  viewer,
};

fn template() -> Article {
  Article {
    id: 0,
    title: String::new(),
    pages: 1,
    published: false,
    note: None,
    author_id: None,
    secret: "s3cret".into(),
    version: 0,
  }
}

fn articles() -> EntityAdmin<Article> {
  EntityAdmin::default()
    .list(["id", "title", "pages"])
    .search(["title"])
    .exclude(["secret"])
    .new_record(template)
    .validate(|a: &Article| {
      if a.pages < 1 {
        Err(ParseErrors::one(ParseError::new("pages", "must be >= 1")))
      } else {
        Ok(())
      }
    })
}

fn site_with(articles: EntityAdmin<Article>) -> AdminSite<Guarded> {
  guarded(
    AdminSite::new()
      .register::<Article>(articles)
      .register::<Author>(EntityAdmin::default().search(["name"]))
      .register::<Tag>(EntityAdmin::default())
      .register::<Note>(EntityAdmin::default()),
  )
}

fn site() -> AdminSite<Guarded> {
  site_with(articles())
}

async fn setup() -> (Db, axum::Router, Session) {
  let db = seeded().await;
  let app = app(site(), &db);
  let session = Session::open(&app, editor(), "/admin/article/@new").await;
  (db, app, session)
}

fn text(row: &id_effect_sql::SqlRow, column: &str) -> String {
  row.get::<String>(column).unwrap()
}

fn int(row: &id_effect_sql::SqlRow, column: &str) -> i64 {
  row.get::<i64>(column).unwrap()
}

#[tokio::test]
async fn admin_create_update_delete_round_trip() {
  let (db, app, ed) = setup().await;

  // The create form: the template's values, the CSRF token, no secret.
  let page = ed.get(&app, "/admin/article/@new").await;
  assert_eq!(page.status, StatusCode::OK);
  assert!(page.body.contains(r#"name="title""#) && page.body.contains(r#"name="pages""#));
  assert!(page.body.contains(r#"value="1""#), "{}", page.body);
  assert!(!page.body.contains(r#"name="secret""#));
  assert!(page.body.contains(r#"name="csrf_token""#));

  // Create.
  let body = form(&[
    ("title", "Fourth"),
    ("pages", "40"),
    ("published", "true"),
    ("note", ""),
    ("author_id", "2"),
  ]);
  let created = ed.post(&app, "/admin/article/@new", &body).await;
  assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
  assert_eq!(created.location(), Some("/admin/article/10"));
  let row = article(&db, 10).await.expect("created");
  assert_eq!(text(&row, "title"), "Fourth");
  assert_eq!(int(&row, "pages"), 40);
  assert_eq!(int(&row, "author_id"), 2);
  assert_eq!(text(&row, "secret"), "s3cret");
  let org = db
    .rows("SELECT org_id FROM ed_articles WHERE id = 10")
    .await;
  assert_eq!(int(&org[0], "org_id"), 1);

  // The detail page links to the edit and delete pages.
  let detail = ed.get(&app, "/admin/article/10").await;
  assert!(detail.body.contains(r#"href="/admin/article/10/@edit""#));
  assert!(detail.body.contains(r#"href="/admin/article/10/@delete""#));

  // The edit form carries the values and the version it was rendered with.
  let edit = ed.get(&app, "/admin/article/10/@edit").await;
  assert_eq!(edit.status, StatusCode::OK);
  assert!(edit.body.contains(r#"value="Fourth""#));
  assert!(
    edit.body.contains(r#"name="_version" value="0""#),
    "{}",
    edit.body
  );

  // Update.
  let body = form(&[
    ("title", "Fourth, revised"),
    ("pages", "41"),
    ("published", "false"),
    ("note", "checked"),
    ("author_id", ""),
    ("_version", "0"),
  ]);
  let updated = ed.post(&app, "/admin/article/10/@edit", &body).await;
  assert_eq!(updated.status, StatusCode::SEE_OTHER, "{}", updated.body);
  assert_eq!(updated.location(), Some("/admin/article/10"));
  let row = article(&db, 10).await.unwrap();
  assert_eq!(text(&row, "title"), "Fourth, revised");
  assert_eq!(text(&row, "note"), "checked");
  assert_eq!(row.get::<Option<i64>>("author_id").unwrap(), None);
  assert_eq!(int(&row, "version"), 1);

  // A stale version (the form of before the update) is a conflict, nothing written.
  let stale = ed
    .post(
      &app,
      "/admin/article/10/@edit",
      &form(&[
        ("title", "late"),
        ("pages", "1"),
        ("published", "true"),
        ("_version", "0"),
      ]),
    )
    .await;
  assert_eq!(stale.status, StatusCode::CONFLICT);
  let bad_version = ed
    .post(
      &app,
      "/admin/article/10/@edit",
      &form(&[("title", "late"), ("_version", "x")]),
    )
    .await;
  assert_eq!(bad_version.status, StatusCode::CONFLICT);
  assert_eq!(
    text(&article(&db, 10).await.unwrap(), "title"),
    "Fourth, revised"
  );

  // Nothing changed: no write.
  let same = ed
    .post(
      &app,
      "/admin/article/10/@edit",
      &form(&[
        ("title", "Fourth, revised"),
        ("pages", "41"),
        ("published", "false"),
        ("note", "checked"),
        ("author_id", ""),
      ]),
    )
    .await;
  assert_eq!(same.status, StatusCode::SEE_OTHER);
  assert_eq!(int(&article(&db, 10).await.unwrap(), "version"), 1);

  // Delete asks for confirmation first.
  let confirm = ed.get(&app, "/admin/article/10/@delete").await;
  assert_eq!(confirm.status, StatusCode::OK);
  assert!(confirm.body.contains("cannot be undone"));
  assert!(
    confirm
      .body
      .contains(r#"action="/admin/article/10/@delete""#)
  );
  let deleted = ed.post(&app, "/admin/article/10/@delete", "").await;
  assert_eq!(deleted.status, StatusCode::SEE_OTHER);
  assert_eq!(deleted.location(), Some("/admin/article"));
  assert!(article(&db, 10).await.is_none());

  // Gone, or never there: 404.
  for uri in [
    "/admin/article/10/@edit",
    "/admin/article/10/@delete",
    "/admin/article/x/@edit",
  ] {
    assert_eq!(
      ed.get(&app, uri).await.status,
      StatusCode::NOT_FOUND,
      "{uri}"
    );
  }
  let missing = ed.post(&app, "/admin/article/10/@delete", "").await;
  assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_create_with_a_typed_in_id() {
  let (db, app, ed) = setup().await;
  let page = ed.get(&app, "/admin/author/@new").await;
  assert!(page.body.contains(r#"name="id""#) && page.body.contains(r#"name="name""#));
  let empty = ed
    .post(
      &app,
      "/admin/author/@new",
      &form(&[("id", ""), ("name", "Eve")]),
    )
    .await;
  assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
  assert!(empty.body.contains("must not be empty"));
  let created = ed
    .post(
      &app,
      "/admin/author/@new",
      &form(&[("id", "5"), ("name", "Eve")]),
    )
    .await;
  assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
  assert_eq!(created.location(), Some("/admin/author/5"));
  let rows = db
    .rows("SELECT name, org_id FROM ed_authors WHERE id = 5")
    .await;
  assert_eq!(text(&rows[0], "name"), "Eve");
  assert_eq!(int(&rows[0], "org_id"), 1);
}

#[tokio::test]
async fn admin_form_ignores_undeclared_fields() {
  let (db, app, ed) = setup().await;
  // Forged fields: the secret, the version, the id, the tenant column.
  let body = form(&[
    ("title", "Renamed"),
    ("secret", "pwned"),
    ("version", "99"),
    ("id", "77"),
    ("org_id", "2"),
  ]);
  let res = ed.post(&app, "/admin/article/2/@edit", &body).await;
  assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
  let row = article(&db, 2).await.unwrap();
  assert_eq!(text(&row, "title"), "Renamed");
  assert_eq!(text(&row, "secret"), "hunter2");
  assert_eq!(int(&row, "version"), 2);
  assert!(article(&db, 77).await.is_none());
  let org = db.rows("SELECT org_id FROM ed_articles WHERE id = 2").await;
  assert_eq!(int(&org[0], "org_id"), 1);
  // A forged create binds only the form fields too.
  let created = ed
    .post(
      &app,
      "/admin/article/@new",
      &form(&[
        ("title", "New"),
        ("pages", "2"),
        ("published", "false"),
        ("secret", "pwned"),
        ("id", "500"),
      ]),
    )
    .await;
  assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
  assert!(article(&db, 500).await.is_none());
  let row = article(&db, 10).await.unwrap();
  assert_eq!(text(&row, "secret"), "s3cret");
  // What the guard reports: the undeclared names, once each.
  let bound = decode_form(body.as_bytes())
    .unwrap()
    .bind(&["title", "pages"]);
  assert_eq!(bound.ignored(), ["secret", "version", "id", "org_id"]);
}

#[tokio::test]
async fn admin_validation_errors_rendered() {
  let (db, app, ed) = setup().await;
  // Parse errors and the entity's validation, each bound to its field.
  let body = form(&[
    ("title", "<b>bold</b>"),
    ("pages", "many"),
    ("published", ""),
  ]);
  let res = ed.post(&app, "/admin/article/@new", &body).await;
  assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
  assert!(res.body.contains("Please correct the errors below."));
  assert!(res.body.contains(r#"data-field="pages""#));
  assert!(
    res
      .body
      .contains(r#"data-message-key="schema-expected-i64""#)
  );
  assert!(res.body.contains("expected bool, got null"));
  // The submitted text is kept, escaped.
  assert!(res.body.contains("&lt;b&gt;bold&lt;/b&gt;"));
  assert!(!res.body.contains("<b>bold"));
  let zero = ed
    .post(
      &app,
      "/admin/article/1/@edit",
      &form(&[("title", "x"), ("pages", "0"), ("published", "true")]),
    )
    .await;
  assert_eq!(zero.status, StatusCode::UNPROCESSABLE_ENTITY);
  assert!(zero.body.contains("must be &gt;= 1"), "{}", zero.body);
  assert_eq!(int(&article(&db, 1).await.unwrap(), "pages"), 10);
  // A `#[from_row(schema)]` entity: its Schema's refinements run on the submitted row.
  let note = ed
    .post(&app, "/admin/note/@new", &form(&[("body", "")]))
    .await;
  assert_eq!(note.status, StatusCode::UNPROCESSABLE_ENTITY);
  assert!(note.body.contains("must not be empty"));
  assert!(note.body.contains(r#"data-field="body""#));
  assert!(db.rows("SELECT id FROM ed_notes").await.is_empty());
  let ok = ed
    .post(&app, "/admin/note/@new", &form(&[("body", "hi")]))
    .await;
  assert_eq!(ok.status, StatusCode::SEE_OTHER);
  // Nothing was audited for the refused forms.
  #[cfg(feature = "audit")]
  assert_eq!(db.audit_rows().len(), 1);
}

#[cfg(feature = "i18n")]
#[tokio::test]
async fn admin_validation_errors_translated() {
  use id_effect_i18n::{Catalog, Locale, SchemaMessages};
  let db = seeded().await;
  let catalog = Catalog::builder(Locale::parse("en").unwrap())
    .schema_messages()
    .use_isolating(false)
    .build()
    .unwrap();
  let site = guarded(
    AdminSite::new()
      .register::<Article>(articles())
      .register::<Note>(EntityAdmin::default())
      .catalog(catalog),
  );
  let app = app(site, &db);
  let ed = Session::open(&app, editor(), "/admin/note/@new").await;
  let res = AdminRequest::post("/admin/note/@new", form(&[("body", "")]))
    .principal(editor())
    .tenant(1i64)
    .extension(Locale::parse("de").unwrap())
    .csrf(&ed.cookie, &ed.token)
    .send(&app)
    .await;
  assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
  assert!(res.body.contains("Darf nicht leer sein."), "{}", res.body);
  // The message key stays the schema's.
  assert!(res.body.contains("schema-must-not-be-empty"));
}

#[tokio::test]
async fn r4_admin_form_over_cap_413() {
  let (db, app, ed) = setup().await;
  let big = format!("title={}", "a".repeat(MAX_FORM_BYTES));
  // The token travels in the header, so only the admin's own cap reads the body.
  let res = ed.post(&app, "/admin/article/1/@edit", &big).await;
  assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
  let res = ed.post(&app, "/admin/article/@new", &big).await;
  assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
  assert_eq!(text(&article(&db, 1).await.unwrap(), "title"), "First");
  // Not a form: 415. Malformed: 400.
  let mut json = AdminRequest::post("/admin/article/1/@edit", "{}")
    .principal(editor())
    .tenant(1i64)
    .csrf(&ed.cookie, &ed.token)
    .into_request();
  json.headers_mut().insert(
    "content-type",
    axum::http::HeaderValue::from_static("application/json"),
  );
  let json = tower::ServiceExt::oneshot(app.clone(), json).await.unwrap();
  assert_eq!(json.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
  let malformed = ed.post(&app, "/admin/article/1/@edit", "title=%zz").await;
  assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn r9_admin_post_without_csrf_token_refused() {
  let (db, app, ed) = setup().await;
  let body = form(&[("title", "forged"), ("pages", "1"), ("published", "true")]);
  // Behind the SecurityLayer: no token, a wrong token, no cookie.
  let no_token = AdminRequest::post("/admin/article/1/@edit", body.clone())
    .principal(editor())
    .tenant(1i64)
    .header(
      "cookie",
      axum::http::HeaderValue::from_str(&ed.cookie).unwrap(),
    )
    .send(&app)
    .await;
  assert_eq!(no_token.status, StatusCode::FORBIDDEN);
  let wrong = AdminRequest::post("/admin/article/1/@edit", body.clone())
    .principal(editor())
    .tenant(1i64)
    .csrf(&ed.cookie, "AAAA")
    .send(&app)
    .await;
  assert_eq!(wrong.status, StatusCode::FORBIDDEN);
  let no_cookie = AdminRequest::post("/admin/article/@new", body.clone())
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(no_cookie.status, StatusCode::FORBIDDEN);
  // Without the SecurityLayer no CSRF check ran: the admin refuses every write itself.
  let bare = bare_app(site(), &db);
  for (uri, body) in [
    ("/admin/article/1/@edit", body.as_str()),
    ("/admin/article/@new", body.as_str()),
    ("/admin/article/1/@delete", ""),
    ("/admin/article/@bulk", "action=delete&id=1"),
    ("/admin/article/1/@rel/tags", "op=add&target=2"),
  ] {
    let res = AdminRequest::post(uri, body)
      .principal(editor())
      .tenant(1i64)
      .send(&bare)
      .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{uri}");
    assert!(res.body.contains("This form has expired."), "{uri}");
  }
  assert_eq!(text(&article(&db, 1).await.unwrap(), "title"), "First");
  assert!(article(&db, 10).await.is_none());
  // The token is in the form when the page is rendered behind the layer.
  let token_field = ed.get(&app, "/admin/article/1/@edit").await;
  assert!(token_field.body.contains(r#"name="csrf_token""#));
}

#[tokio::test]
async fn admin_writes_require_their_permission() {
  let (db, app, _) = setup().await;
  // The viewer's list page has no "New" link and no bulk form; the detail no edit links.
  let res = AdminRequest::get("/admin/article")
    .principal(viewer())
    .tenant(1i64)
    .send(&app)
    .await;
  assert!(!res.body.contains("/admin/article/@new"));
  assert!(!res.body.contains("admin-bulk"));
  let detail = AdminRequest::get("/admin/article/1")
    .principal(viewer())
    .tenant(1i64)
    .send(&app)
    .await;
  assert!(!detail.body.contains("@edit") && !detail.body.contains("@delete"));
  for uri in [
    "/admin/article/@new",
    "/admin/article/1/@edit",
    "/admin/article/1/@delete",
  ] {
    let res = AdminRequest::get(uri)
      .principal(viewer())
      .tenant(1i64)
      .send(&app)
      .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{uri}");
  }
  let editor_list = AdminRequest::get("/admin/article")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert!(editor_list.body.contains(r#"href="/admin/article/@new""#));
  // A viewer's session cannot write, CSRF token or not.
  let ed_page = AdminRequest::get("/admin/article/1/@edit")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  let (cookie, token) = (
    ed_page.csrf_cookie().unwrap(),
    ed_page.csrf_token().unwrap(),
  );
  for uri in [
    "/admin/article/@new",
    "/admin/article/1/@edit",
    "/admin/article/1/@delete",
  ] {
    let res = AdminRequest::post(uri, "title=x")
      .principal(viewer())
      .tenant(1i64)
      .csrf(&cookie, &token)
      .send(&app)
      .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{uri}");
  }
  // Anonymous (with a valid CSRF session): 401 from the guard.
  let anonymous = AdminRequest::post("/admin/article/@new", "title=x")
    .csrf(&cookie, &token)
    .send(&app)
    .await;
  assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
  // An editor without a tenant cannot write a tenant-scoped entity.
  let no_tenant = AdminRequest::post("/admin/article/@new", "title=x")
    .principal(editor())
    .csrf(&cookie, &token)
    .send(&app)
    .await;
  assert_eq!(no_tenant.status, StatusCode::FORBIDDEN);
  assert_eq!(text(&article(&db, 1).await.unwrap(), "title"), "First");
}

#[tokio::test]
async fn admin_writes_run_lifecycle_hooks() {
  let db = seeded().await;
  let hooks = EntityHooks::<Article>::new()
    .before_insert(|_ctx, a| {
      succeed(Article {
        title: a.title.trim().to_owned(),
        ..a
      })
    })
    .before_update(|_ctx, a| {
      if a.title == "reject" {
        fail(SqlError::Unsupported("rejected by a hook".into()))
      } else {
        succeed(a)
      }
    })
    .before_delete(|_ctx, id| {
      if *id == 3 {
        fail(SqlError::Unsupported("article 3 is kept".into()))
      } else {
        succeed(())
      }
    });
  let app = app(site_with(articles().hooks(hooks)), &db);
  let ed = Session::open(&app, editor(), "/admin/article/@new").await;
  let created = ed
    .post(
      &app,
      "/admin/article/@new",
      &form(&[
        ("title", "  padded  "),
        ("pages", "3"),
        ("published", "false"),
      ]),
    )
    .await;
  assert_eq!(created.status, StatusCode::SEE_OTHER);
  assert_eq!(text(&article(&db, 10).await.unwrap(), "title"), "padded");
  // A failing hook rolls the write (and its audit record) back.
  #[cfg(feature = "audit")]
  let audited = db.audit_rows().len();
  let rejected = ed
    .post(
      &app,
      "/admin/article/1/@edit",
      &form(&[("title", "reject"), ("pages", "10"), ("published", "true")]),
    )
    .await;
  assert_eq!(rejected.status, StatusCode::INTERNAL_SERVER_ERROR);
  assert_eq!(text(&article(&db, 1).await.unwrap(), "title"), "First");
  let kept = ed.post(&app, "/admin/article/3/@delete", "").await;
  assert_eq!(kept.status, StatusCode::INTERNAL_SERVER_ERROR);
  assert!(article(&db, 3).await.is_some());
  #[cfg(feature = "audit")]
  assert_eq!(db.audit_rows().len(), audited);
}

#[tokio::test]
async fn admin_create_needs_every_required_field() {
  let db = seeded().await;
  // Without a template the secret (required, not editable) has no value: no create page.
  let app = app(site_with(EntityAdmin::default().exclude(["secret"])), &db);
  let res = AdminRequest::get("/admin/article/@new")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(res.status, StatusCode::NOT_FOUND);
  let list = AdminRequest::get("/admin/article")
    .principal(editor())
    .tenant(1i64)
    .send(&app)
    .await;
  assert!(!list.body.contains("@new"));
  let ed = Session::open(&app, editor(), "/admin/article/1/@edit").await;
  let post = ed.post(&app, "/admin/article/@new", "title=x").await;
  assert_eq!(post.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_create_lands_on_the_list_without_view() {
  use id_effect_authz::{Guard, Rbac, require_role};
  let db = seeded().await;
  let rbac = Rbac::builder()
    .grant("creator", ["admin.tag.list", "admin.tag.create"])
    .build();
  let site = AdminSite::new()
    .register::<Tag>(EntityAdmin::default())
    .rbac(rbac)
    .guard(Guard::new("staff", require_role("staff")));
  let app = app(site, &db);
  let creator = id_effect_authz::BasicPrincipal::new("cr")
    .with_role("staff")
    .with_role("creator");
  let session = Session::open(&app, creator, "/admin/tag/@new").await;
  // No template: the nullable colour is NULL, the tenant field is the request's tenant.
  let res = session
    .post(
      &app,
      "/admin/tag/@new",
      &form(&[("id", "7"), ("name", "go")]),
    )
    .await;
  assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
  assert_eq!(res.location(), Some("/admin/tag"));
  let rows = db
    .rows("SELECT org_id, name, color FROM ed_tags WHERE id = 7")
    .await;
  assert_eq!(int(&rows[0], "org_id"), 1);
  assert_eq!(rows[0].get::<Option<String>>("color").unwrap(), None);
}

#[test]
fn admin_form_configuration_is_checked() {
  let not_editable = EntityAdmin::<Article>::default().form(["secret"]).info();
  assert!(matches!(
    not_editable,
    Err(AdminConfigError::NotEditable { ref field, .. }) if field == "secret"
  ));
  let excluded = EntityAdmin::<Article>::default()
    .exclude(["note"])
    .form(["note"])
    .info();
  assert!(matches!(
    excluded,
    Err(AdminConfigError::NotEditable { .. })
  ));
  let unknown = EntityAdmin::<Article>::default().form(["nope"]).info();
  assert!(matches!(
    unknown,
    Err(AdminConfigError::UnknownField { .. })
  ));
  let info = EntityAdmin::<Article>::default()
    .form(["title", "pages"])
    .info()
    .unwrap();
  assert_eq!(info.form, ["title", "pages"]);
  assert_eq!(info.relations, ["author", "tags"]);
  let all = EntityAdmin::<Article>::default().info().unwrap();
  assert_eq!(
    all.form,
    ["title", "pages", "published", "note", "author_id"]
  );
  let message = AdminConfigError::NotEditable {
    entity: "Article",
    field: "secret".into(),
  }
  .to_string();
  assert!(message.contains("`secret` is not an editable field"));
}

#[tokio::test]
async fn admin_form_only_fields_restrict_the_form() {
  let db = seeded().await;
  let app = app(site_with(articles().form(["title"])), &db);
  let ed = Session::open(&app, editor(), "/admin/article/1/@edit").await;
  let page = ed.get(&app, "/admin/article/1/@edit").await;
  assert!(page.body.contains(r#"name="title""#) && !page.body.contains(r#"name="pages""#));
  let res = ed
    .post(
      &app,
      "/admin/article/1/@edit",
      &form(&[("title", "T"), ("pages", "99")]),
    )
    .await;
  assert_eq!(res.status, StatusCode::SEE_OTHER);
  let row = article(&db, 1).await.unwrap();
  assert_eq!(
    (text(&row, "title").as_str(), int(&row, "pages")),
    ("T", 10)
  );
}

#[cfg(feature = "audit")]
#[tokio::test]
async fn admin_change_written_to_audit_log() {
  use id_effect_sql::audit::AuditRecord;
  use id_effect_sql::serde_json::json;
  let (db, app, ed) = setup().await;
  let created = ed
    .post(
      &app,
      "/admin/article/@new",
      &form(&[("title", "Audited"), ("pages", "4"), ("published", "true")]),
    )
    .await;
  assert_eq!(created.status, StatusCode::SEE_OTHER);
  let updated = ed
    .post(
      &app,
      "/admin/article/10/@edit",
      &form(&[
        ("title", "Audited twice"),
        ("pages", "4"),
        ("published", "true"),
      ]),
    )
    .await;
  assert_eq!(updated.status, StatusCode::SEE_OTHER);
  let deleted = ed.post(&app, "/admin/article/10/@delete", "").await;
  assert_eq!(deleted.status, StatusCode::SEE_OTHER);

  let records: Vec<AuditRecord> = db
    .audit_rows()
    .iter()
    .map(|row| AuditRecord::from_row(row).unwrap())
    .collect();
  assert_eq!(records.len(), 3);
  let actions: Vec<&str> = records.iter().map(|r| r.action.as_str()).collect();
  assert_eq!(actions, ["admin.create", "admin.update", "admin.delete"]);
  for r in &records {
    assert_eq!(r.actor, "ed");
    assert_eq!(r.tenant.as_deref(), Some("1"));
    assert_eq!(r.entity, "article:10");
    // The secret column never reaches the log.
    let text = format!("{:?}{:?}", r.before, r.after);
    assert!(
      !text.contains("secret") && !text.contains("s3cret"),
      "{text}"
    );
  }
  assert_eq!(
    records[0].after,
    Some(
      json!({"title": "Audited", "pages": 4, "published": true, "note": null, "author_id": null})
    )
  );
  // An update records the changed fields only.
  assert_eq!(records[1].before, Some(json!({"title": "Audited"})));
  assert_eq!(records[1].after, Some(json!({"title": "Audited twice"})));
  assert_eq!(
    records[2].before.as_ref().unwrap()["title"],
    json!("Audited twice")
  );
  // The records form an intact hash chain.
  let mut prev = vec![0u8; 32];
  for (n, r) in records.iter().enumerate() {
    assert_eq!(r.seq, n as i64 + 1);
    assert_eq!(r.prev_hash, prev);
    assert_eq!(r.computed_hash(), r.hash);
    prev = r.hash.clone();
  }
}

#[cfg(feature = "audit")]
#[tokio::test]
async fn admin_audit_can_be_turned_off_only_on_record() {
  let db = seeded().await;
  let site = site().dangerous_without_audit();
  let opted_out = id_effect::observability::posture::dangerous_opt_outs();
  assert!(
    opted_out
      .iter()
      .any(|(id, _)| *id == "id_effect_admin::AdminSite::dangerous_without_audit"),
    "{opted_out:?}"
  );
  let app = app(site, &db);
  let ed = Session::open(&app, editor(), "/admin/article/@new").await;
  let res = ed
    .post(&app, "/admin/article/1/@edit", &form(&[("title", "quiet")]))
    .await;
  assert_eq!(res.status, StatusCode::SEE_OTHER);
  assert!(db.audit_rows().is_empty());
  // Another log table is a builder call away.
  let other = guarded(AdminSite::new().register::<Article>(articles()))
    .audit_log(id_effect_sql::audit::AuditLog::new().with_table("other_audit"));
  assert_eq!(other.entities().len(), 1);
}
