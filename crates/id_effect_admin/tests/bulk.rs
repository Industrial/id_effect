//! Bulk actions on SQLite behind `SecurityLayer`: the list page's selection, the built-in
//! delete, a custom action, the 100-id cap, both permissions and the audit record.
//!
//! ADR 0013: `r4_admin_bulk_action_over_cap_rejected` (R4).

mod support;

use axum::http::StatusCode;
use id_effect::succeed;
use id_effect_admin::bulk::{BulkAction, MAX_BULK_IDS};
use id_effect_admin::testing::AdminRequest;
use id_effect_admin::{AdminAction, AdminConfigError, AdminSite, EntityAdmin, Guarded};
use id_effect_authz::{BasicPrincipal, Guard, Rbac, require_role};
use support::{Article, Db, Session, app, article, editor, perms, seeded};

fn publish() -> BulkAction<Article> {
  BulkAction::new("publish", "Publish", |ctx, rows: Vec<Article>| {
    assert_eq!(ctx.principal(), "ed");
    assert!(format!("{ctx:?}").contains("ed"));
    let _tx = ctx.transaction();
    let _repo = ctx.repository();
    rows.into_iter().fold(succeed(()), |done, article| {
      let update = ctx.update(Article {
        published: true,
        ..article
      });
      done.flat_map(move |()| update.map(|_| ()))
    })
  })
}

fn site() -> AdminSite<Guarded> {
  let rbac = Rbac::builder()
    .grant("editor", perms("article"))
    .grant(
      "bulker",
      [
        "admin.article.list",
        "admin.article.view",
        "admin.article.bulk",
      ],
    )
    .build();
  AdminSite::new()
    .register::<Article>(
      EntityAdmin::default()
        .list(["id", "title"])
        .action(publish())
        .action(
          BulkAction::new("archive", "Archive", |_, _: Vec<Article>| succeed(()))
            .requires(AdminAction::Delete),
        ),
    )
    .rbac(rbac)
    .guard(Guard::new("staff", require_role("staff")))
}

async fn setup() -> (Db, axum::Router, Session) {
  let db = seeded().await;
  let app = app(site(), &db);
  let session = Session::open(&app, editor(), "/admin/article/1/@edit").await;
  (db, app, session)
}

fn ids(ids: impl IntoIterator<Item = i64>) -> String {
  ids
    .into_iter()
    .map(|id| format!("&id={id}"))
    .collect::<String>()
}

fn published(row: &id_effect_sql::SqlRow) -> bool {
  row.get::<bool>("published").unwrap()
}

#[tokio::test]
async fn admin_list_offers_bulk_actions() {
  let (_db, app, ed) = setup().await;
  let list = ed.get(&app, "/admin/article").await;
  assert!(
    list.body.contains(r#"action="/admin/article/@bulk""#),
    "{}",
    list.body
  );
  assert!(
    list
      .body
      .contains(r#"<input type="checkbox" name="id" value="1""#)
  );
  assert!(
    list
      .body
      .contains(r#"<option value="delete">Delete</option>"#)
  );
  assert!(
    list
      .body
      .contains(r#"<option value="publish">Publish</option>"#)
  );
  assert!(list.body.contains(r#"name="csrf_token""#));
  // A principal with `bulk` but neither `update` nor `delete` has no action, so no form.
  let bulker = BasicPrincipal::new("bu")
    .with_role("staff")
    .with_role("bulker");
  let list = AdminRequest::get("/admin/article")
    .principal(bulker)
    .tenant(1i64)
    .send(&app)
    .await;
  assert!(!list.body.contains("admin-bulk"));
}

#[tokio::test]
async fn admin_bulk_custom_action() {
  let (db, app, ed) = setup().await;
  // Article 9 belongs to org 2: selecting it selects nothing.
  let body = format!("action=publish{}", ids([2, 3, 9]));
  let res = ed.post(&app, "/admin/article/@bulk", &body).await;
  assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
  assert_eq!(res.location(), Some("/admin/article"));
  assert!(published(&article(&db, 2).await.unwrap()));
  assert!(published(&article(&db, 3).await.unwrap()));
  assert!(!published(&article(&db, 9).await.unwrap()));
  // The update went through the repository: optimistic locking bumped the versions.
  assert_eq!(
    article(&db, 2)
      .await
      .unwrap()
      .get::<i64>("version")
      .unwrap(),
    2
  );
  #[cfg(feature = "audit")]
  {
    use id_effect_sql::audit::AuditRecord;
    let rows = db.audit_rows();
    let record = AuditRecord::from_row(rows.last().unwrap()).unwrap();
    assert_eq!(record.action, "admin.bulk.publish");
    assert_eq!(record.entity, "article");
    assert_eq!(record.actor, "ed");
    assert_eq!(
      record.after,
      Some(id_effect_sql::serde_json::json!({"ids": ["2", "3"]}))
    );
  }
  // Each action checks its own permission besides `bulk`.
  let bulker = BasicPrincipal::new("bu")
    .with_role("staff")
    .with_role("bulker");
  for action in ["publish", "delete", "archive"] {
    let res = AdminRequest::post("/admin/article/@bulk", format!("action={action}&id=1"))
      .principal(bulker.clone())
      .tenant(1i64)
      .csrf(&ed.cookie, &ed.token)
      .send(&app)
      .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{action}");
  }
  assert_eq!(
    article(&db, 1)
      .await
      .unwrap()
      .get::<i64>("version")
      .unwrap(),
    1
  );
}

#[tokio::test]
async fn admin_bulk_delete() {
  let (db, app, ed) = setup().await;
  let res = ed
    .post(
      &app,
      "/admin/article/@bulk",
      &format!("action=delete{}", ids([2, 3])),
    )
    .await;
  assert_eq!(res.status, StatusCode::SEE_OTHER);
  assert!(article(&db, 2).await.is_none() && article(&db, 3).await.is_none());
  assert!(article(&db, 1).await.is_some());
  #[cfg(feature = "audit")]
  {
    use id_effect_sql::audit::AuditRecord;
    let record = AuditRecord::from_row(db.audit_rows().last().unwrap()).unwrap();
    assert_eq!(record.action, "admin.bulk.delete");
  }
  // No selection: back to the list, nothing done.
  let none = ed.post(&app, "/admin/article/@bulk", "action=delete").await;
  assert_eq!(none.status, StatusCode::SEE_OTHER);
  // An unknown action, a malformed id: 400.
  let unknown = ed
    .post(&app, "/admin/article/@bulk", "action=explode&id=1")
    .await;
  assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
  let bad_id = ed
    .post(&app, "/admin/article/@bulk", "action=delete&id=one")
    .await;
  assert_eq!(bad_id.status, StatusCode::BAD_REQUEST);
  assert!(article(&db, 1).await.is_some());
}

#[tokio::test]
async fn r4_admin_bulk_action_over_cap_rejected() {
  let (db, app, ed) = setup().await;
  let over = format!("action=delete{}", ids(1..=(MAX_BULK_IDS as i64 + 1)));
  let res = ed.post(&app, "/admin/article/@bulk", &over).await;
  assert_eq!(res.status, StatusCode::BAD_REQUEST);
  assert!(res.body.contains("Too many records are selected."));
  assert!(article(&db, 1).await.is_some());
  #[cfg(feature = "audit")]
  assert!(db.audit_rows().is_empty());
  // The cap itself is allowed.
  let at_cap = format!("action=delete{}", ids(1..=(MAX_BULK_IDS as i64)));
  let res = ed.post(&app, "/admin/article/@bulk", &at_cap).await;
  assert_eq!(res.status, StatusCode::SEE_OTHER);
  assert!(article(&db, 1).await.is_none());
  // Another tenant's row in the range is untouched.
  assert!(article(&db, 9).await.is_some());
}

#[test]
fn admin_bulk_action_names_are_checked() {
  let noop = |name: &str| BulkAction::new(name, name, |_, _: Vec<Article>| succeed(()));
  for name in ["delete", "Bad-Name", ""] {
    let err = EntityAdmin::<Article>::default().action(noop(name)).info();
    assert!(
      matches!(err, Err(AdminConfigError::InvalidAction { ref action, .. }) if action == name),
      "{name}"
    );
  }
  let twice = EntityAdmin::<Article>::default()
    .action(noop("archive"))
    .action(noop("archive"))
    .info();
  assert!(matches!(twice, Err(AdminConfigError::InvalidAction { .. })));
  assert!(
    AdminConfigError::InvalidAction {
      entity: "Article",
      action: "delete".into()
    }
    .to_string()
    .contains("bulk action `delete`")
  );
  let action = noop("archive").requires(AdminAction::Delete);
  assert_eq!(action.name(), "archive");
  assert!(format!("{action:?}").contains("Delete"));
}
