//! Relations in the admin on SQLite behind `SecurityLayer`: `belongs_to` pickers, `has_many`
//! inline lists and `many_to_many` add / remove, all inside the request's tenant.
//!
//! ADR 0013: `admin_fk_picker_lists_only_tenant_rows` (R1).

mod support;

use axum::http::StatusCode;
use id_effect_admin::testing::AdminRequest;
use id_effect_admin::{AdminSite, EntityAdmin, Guarded};
use id_effect_authz::{BasicPrincipal, Guard, Rbac, require_role};
use support::{
  Article, Author, Db, Session, Tag, app, article, editor, form, guarded, perms, seeded, viewer,
};

fn site() -> AdminSite<Guarded> {
  guarded(
    AdminSite::new()
      .register::<Article>(EntityAdmin::default().exclude(["secret"]))
      .register::<Author>(EntityAdmin::default().search(["name"]))
      .register::<Tag>(EntityAdmin::default()),
  )
}

async fn setup() -> (Db, axum::Router, Session) {
  let db = seeded().await;
  let app = app(site(), &db);
  let session = Session::open(&app, editor(), "/admin/article/1/@edit").await;
  (db, app, session)
}

async fn tags_of(db: &Db, article: i64) -> Vec<i64> {
  db.rows(&format!(
    "SELECT tag_id FROM ed_article_tags WHERE article_id = {article} ORDER BY tag_id"
  ))
  .await
  .iter()
  .map(|r| r.get::<i64>("tag_id").unwrap())
  .collect()
}

#[tokio::test]
async fn admin_fk_picker_lists_only_tenant_rows() {
  let (db, app, ed) = setup().await;
  // The edit form's picker: org 1's authors, the current one selected; never org 2's.
  let page = ed.get(&app, "/admin/article/1/@edit").await;
  assert!(page.body.contains("admin-picker"), "{}", page.body);
  assert!(
    page
      .body
      .contains(r#"hx-get="/admin/article/@pick/author""#)
  );
  assert!(
    page
      .body
      .contains(r#"<option value="1" selected>Ada</option>"#)
  );
  assert!(page.body.contains(r#"<option value="2">Brian</option>"#));
  assert!(!page.body.contains("Globex"));
  // The picker's search, in org 1 and in org 2.
  let all = ed.get(&app, "/admin/article/@pick/author?_q=").await;
  assert_eq!(all.status, StatusCode::OK);
  assert!(all.body.contains("Ada") && all.body.contains("Brian"));
  assert!(!all.body.contains("Globex"));
  assert!(all.body.starts_with(r#"<option value="""#), "{}", all.body);
  let bri = ed.get(&app, "/admin/article/@pick/author?_q=BRI").await;
  assert!(bri.body.contains("Brian") && !bri.body.contains("Ada"));
  let org2 = AdminRequest::get("/admin/article/@pick/author")
    .principal(editor())
    .tenant(2i64)
    .send(&app)
    .await;
  assert!(org2.body.contains("Globex") && !org2.body.contains("Ada"));
  // A foreign key naming another tenant's row is refused on the form.
  let forged = ed
    .post(
      &app,
      "/admin/article/1/@edit",
      &form(&[
        ("title", "First"),
        ("pages", "10"),
        ("published", "true"),
        ("author_id", "9"),
      ]),
    )
    .await;
  assert_eq!(forged.status, StatusCode::UNPROCESSABLE_ENTITY);
  assert!(forged.body.contains("must reference an existing record"));
  let row = article(&db, 1).await.unwrap();
  assert_eq!(row.get::<i64>("author_id").unwrap(), 1);
  // One of its own tenant is accepted.
  let ok = ed
    .post(
      &app,
      "/admin/article/1/@edit",
      &form(&[
        ("title", "First"),
        ("pages", "10"),
        ("published", "true"),
        ("author_id", "2"),
      ]),
    )
    .await;
  assert_eq!(ok.status, StatusCode::SEE_OTHER, "{}", ok.body);
  assert_eq!(
    article(&db, 1)
      .await
      .unwrap()
      .get::<i64>("author_id")
      .unwrap(),
    2
  );
}

#[tokio::test]
async fn admin_picker_needs_permissions() {
  let (_db, app, _ed) = setup().await;
  // A viewer may neither create nor update articles.
  let res = AdminRequest::get("/admin/article/@pick/author")
    .principal(viewer())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(res.status, StatusCode::FORBIDDEN);
  // An article editor who may not list authors gets no picker, a plain input instead.
  let rbac = Rbac::builder().grant("writer", perms("article")).build();
  let site = AdminSite::new()
    .register::<Article>(EntityAdmin::default().exclude(["secret"]))
    .register::<Author>(EntityAdmin::default())
    .register::<Tag>(EntityAdmin::default())
    .rbac(rbac)
    .guard(Guard::new("staff", require_role("staff")));
  let db = seeded().await;
  let app = support::app(site, &db);
  let writer = BasicPrincipal::new("wr")
    .with_role("staff")
    .with_role("writer");
  let page = AdminRequest::get("/admin/article/1/@edit")
    .principal(writer.clone())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(page.status, StatusCode::OK);
  assert!(!page.body.contains("admin-picker"));
  assert!(page.body.contains(r#"name="author_id""#));
  let pick = AdminRequest::get("/admin/article/@pick/author")
    .principal(writer.clone())
    .tenant(1i64)
    .send(&app)
    .await;
  assert_eq!(pick.status, StatusCode::FORBIDDEN);
  // The detail page leaves out the relations whose target it may not list.
  let detail = AdminRequest::get("/admin/article/1")
    .principal(writer)
    .tenant(1i64)
    .send(&app)
    .await;
  assert!(!detail.body.contains("admin-relation"));
}

#[tokio::test]
async fn admin_picker_refuses_unknown_and_oversized_requests() {
  let (_db, app, ed) = setup().await;
  assert_eq!(
    ed.get(&app, "/admin/article/@pick/nope").await.status,
    StatusCode::NOT_FOUND
  );
  let long = format!("/admin/article/@pick/author?_q={}", "a".repeat(3000));
  assert_eq!(ed.get(&app, &long).await.status, StatusCode::URI_TOO_LONG);
  assert_eq!(
    ed.get(&app, "/admin/article/@pick/author?_q=%zz")
      .await
      .status,
    StatusCode::BAD_REQUEST
  );
  // The many-to-many relation has a picker too (the add form's).
  let tags = ed.get(&app, "/admin/article/@pick/tags?_q=s").await;
  assert!(tags.body.contains("rust") && tags.body.contains("sql"));
  assert!(!tags.body.contains("secret"));
  assert!(!tags.body.contains(r#"<option value="">"#));
  // A row whose display field is empty is shown by its id.
  let all = ed.get(&app, "/admin/article/@pick/tags?_q=").await;
  assert!(
    all.body.contains(r#"<option value="4">4</option>"#),
    "{}",
    all.body
  );
}

#[tokio::test]
async fn admin_relations_degrade_when_a_target_cannot_be_read() {
  let (db, app, ed) = setup().await;
  db.exec("DROP TABLE ed_tags").await;
  db.exec("DROP TABLE ed_authors").await;
  // The detail page leaves the unreadable list out; the form falls back to a plain input.
  let detail = ed.get(&app, "/admin/article/1").await;
  assert_eq!(detail.status, StatusCode::OK);
  assert!(!detail.body.contains(r#"data-relation="tags""#));
  let edit = ed.get(&app, "/admin/article/1/@edit").await;
  assert_eq!(edit.status, StatusCode::OK);
  assert!(!edit.body.contains("admin-picker") && edit.body.contains(r#"name="author_id""#));
  // A foreign key that cannot be checked is refused (fail closed).
  let res = ed
    .post(&app, "/admin/article/1/@edit", &form(&[("author_id", "2")]))
    .await;
  assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
  assert!(res.body.contains("must reference an existing record"));
}

#[tokio::test]
async fn admin_relations_need_a_registered_target() {
  let db = seeded().await;
  let app = app(
    guarded(AdminSite::new().register::<Article>(EntityAdmin::default().exclude(["secret"]))),
    &db,
  );
  let ed = Session::open(&app, editor(), "/admin/article/1/@edit").await;
  let page = ed.get(&app, "/admin/article/1/@edit").await;
  assert!(!page.body.contains("admin-picker"));
  let detail = ed.get(&app, "/admin/article/1").await;
  assert!(!detail.body.contains("admin-relation"));
  assert_eq!(
    ed.get(&app, "/admin/article/@pick/author").await.status,
    StatusCode::NOT_FOUND
  );
  let res = ed
    .post(&app, "/admin/article/1/@rel/tags", "op=add&target=2")
    .await;
  assert_eq!(res.status, StatusCode::NOT_FOUND);
  // Without the target registered the key is written as given (register it to get the check).
  let res = ed
    .post(&app, "/admin/article/1/@edit", &form(&[("author_id", "2")]))
    .await;
  assert_eq!(res.status, StatusCode::SEE_OTHER);
  assert_eq!(
    article(&db, 1)
      .await
      .unwrap()
      .get::<i64>("author_id")
      .unwrap(),
    2
  );
}

#[tokio::test]
async fn admin_has_many_inline_list() {
  let (_db, app, ed) = setup().await;
  let detail = ed.get(&app, "/admin/author/1").await;
  assert!(
    detail.body.contains(r#"data-relation="articles""#),
    "{}",
    detail.body
  );
  assert!(
    detail
      .body
      .contains(r#"<a href="/admin/article/1">First</a>"#)
  );
  // A has_many list has no add / remove.
  assert!(!detail.body.contains("@rel/articles"));
  let empty = AdminRequest::get("/admin/author/9")
    .principal(editor())
    .tenant(2i64)
    .send(&app)
    .await;
  assert!(empty.body.contains("Globex plan"));
}

#[tokio::test]
async fn admin_many_to_many_add_remove() {
  let (db, app, ed) = setup().await;
  let detail = ed.get(&app, "/admin/article/1").await;
  assert!(detail.body.contains(r#"data-relation="tags""#));
  assert!(detail.body.contains(r#"<a href="/admin/tag/1">rust</a>"#));
  assert!(
    detail
      .body
      .contains(r#"action="/admin/article/1/@rel/tags""#)
  );
  assert!(!detail.body.contains("secret"));

  // Add, twice (the pair is written once).
  for _ in 0..2 {
    let res = ed
      .post(&app, "/admin/article/1/@rel/tags", "op=add&target=2")
      .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), Some("/admin/article/1"));
  }
  assert_eq!(tags_of(&db, 1).await, [1, 2]);
  // Remove.
  let res = ed
    .post(&app, "/admin/article/1/@rel/tags", "op=remove&target=1")
    .await;
  assert_eq!(res.status, StatusCode::SEE_OTHER);
  assert_eq!(tags_of(&db, 1).await, [2]);
  // Removing a pair that is not there changes nothing.
  let res = ed
    .post(&app, "/admin/article/1/@rel/tags", "op=remove&target=3")
    .await;
  assert_eq!(res.status, StatusCode::SEE_OTHER);

  // Another tenant's tag, a malformed target, an unknown op: 400. Another tenant's article: 404.
  for body in [
    "op=add&target=9",
    "op=add&target=x",
    "op=add",
    "op=swap&target=2",
  ] {
    let res = ed.post(&app, "/admin/article/1/@rel/tags", body).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{body}");
  }
  let res = ed
    .post(&app, "/admin/article/9/@rel/tags", "op=add&target=2")
    .await;
  assert_eq!(res.status, StatusCode::NOT_FOUND);
  // Only join-table relations have add / remove.
  let res = ed
    .post(&app, "/admin/article/1/@rel/author", "op=add&target=2")
    .await;
  assert_eq!(res.status, StatusCode::NOT_FOUND);
  let res = ed
    .post(&app, "/admin/article/1/@rel/nope", "op=add&target=2")
    .await;
  assert_eq!(res.status, StatusCode::NOT_FOUND);
  assert_eq!(tags_of(&db, 1).await, [2]);
  assert_eq!(tags_of(&db, 9).await, [9]);
  // A viewer cannot change pairs.
  let res = AdminRequest::post("/admin/article/1/@rel/tags", "op=add&target=3")
    .principal(viewer())
    .tenant(1i64)
    .csrf(&ed.cookie, &ed.token)
    .send(&app)
    .await;
  assert_eq!(res.status, StatusCode::FORBIDDEN);

  #[cfg(feature = "audit")]
  {
    use id_effect_sql::audit::AuditRecord;
    let actions: Vec<(String, Option<id_effect_sql::serde_json::Value>)> = db
      .audit_rows()
      .iter()
      .map(|r| AuditRecord::from_row(r).unwrap())
      .map(|r| (r.action, r.after))
      .collect();
    assert_eq!(
      actions,
      [
        (
          "admin.relation.add".to_owned(),
          Some(id_effect_sql::serde_json::json!({"tags": "2"}))
        ),
        (
          "admin.relation.remove".to_owned(),
          Some(id_effect_sql::serde_json::json!({"tags": "1"}))
        ),
      ]
    );
  }
}
