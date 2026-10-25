//! The generated admin on PostgreSQL (release spec 0.13.0, D1 stage 5; r13-w7-pipeline-backoffice):
//! roles, the audit trail of every change, the bulk re-run of extraction and the tenant-scoped
//! label picker. HTML is read through the in-process HTTP client (`id_effect_test`), with the
//! app's whole router: authentication, tenancy, CSRF and locale layers included.

#[path = "../fixtures/support.rs"]
mod support;

use std::time::Duration;

use id_effect_kit::sql::{SqlClient, SqlValue, params};
use id_effect_kit::tenancy::Tenancy;
use id_effect_kit::test::http::{StatusCode, TestClient, TestResponse};
use pipeline::admin::ROLE_ADMIN;
use pipeline::admin::document::RERUN_DELAY;
use pipeline::domain::{self, NewDocument};
use pipeline::mcp::McpSettings;
use pipeline::stages;
use pipeline::tenancy::{ROLE_EDITOR, ROLE_REVIEWER};
use support::{Harness, extraction, run};

// ---- a browser session over the in-process client ----

/// One signed-in user: a bearer token, the CSRF cookie and the masked token of the last page.
struct Browser {
  client: TestClient<axum::Router>,
  token: Option<String>,
  cookie: Option<String>,
  csrf: Option<String>,
}

fn encode(text: &str) -> String {
  text
    .bytes()
    .map(|b| match b {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
        (b as char).to_string()
      }
      b' ' => "+".to_owned(),
      _ => format!("%{b:02X}"),
    })
    .collect()
}

impl Browser {
  async fn new(h: &Harness, sub: &str, tenant: Option<&str>, roles: &[&str]) -> Self {
    let router = pipeline::router(&h.app, &h.keys, &McpSettings::default()).expect("router");
    let token = if roles.is_empty() && tenant.is_none() {
      None
    } else {
      Some(h.token(sub, tenant, roles).await)
    };
    Self {
      client: TestClient::new(router),
      token,
      cookie: None,
      csrf: None,
    }
  }

  fn remember(&mut self, res: &TestResponse) {
    for value in res.headers().get_all("set-cookie") {
      let value = value.to_str().unwrap_or_default();
      if value.starts_with("__Host-csrf=") {
        self.cookie = value.split(';').next().map(str::to_owned);
      }
    }
    let body = res.text();
    let marker = r#"name="csrf_token" value=""#;
    if let Some(start) = body.find(marker).map(|at| at + marker.len())
      && let Some(len) = body[start..].find('"')
    {
      self.csrf = Some(body[start..start + len].to_owned());
    }
  }

  async fn send(
    &mut self,
    post: Option<&[(&str, &str)]>,
    uri: &str,
    headers: &[(&str, &str)],
  ) -> TestResponse {
    let mut req = match post {
      Some(form) => {
        let mut pairs: Vec<String> = form
          .iter()
          .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
          .collect();
        if let Some(csrf) = &self.csrf {
          pairs.push(format!("csrf_token={}", encode(csrf)));
        }
        self
          .client
          .post(uri)
          .body(pairs.join("&"), "application/x-www-form-urlencoded")
      }
      None => self.client.get(uri),
    };
    if let Some(token) = &self.token {
      req = req.bearer(token);
    }
    if let Some(cookie) = &self.cookie {
      req = req.header("cookie", cookie);
    }
    for (name, value) in headers {
      req = req.header(name, value);
    }
    let res = run(req.send()).await.expect("the request is answered");
    self.remember(&res);
    res
  }

  async fn get(&mut self, uri: &str) -> TestResponse {
    self.send(None, uri, &[]).await
  }

  async fn post(&mut self, uri: &str, form: &[(&str, &str)]) -> TestResponse {
    self.send(Some(form), uri, &[]).await
  }
}

async fn ids(db: &std::sync::Arc<dyn SqlClient>, sql: &str, params: Vec<SqlValue>) -> Vec<i64> {
  run(db.query(sql, &params))
    .await
    .expect("query")
    .iter()
    .map(|row| row.get::<i64>(0usize).expect("an id"))
    .collect()
}

async fn texts(
  db: &std::sync::Arc<dyn SqlClient>,
  sql: &str,
  params: Vec<SqlValue>,
) -> Vec<String> {
  run(db.query(sql, &params))
    .await
    .expect("query")
    .iter()
    .map(|row| row.get::<String>(0usize).expect("a text"))
    .collect()
}

const AUDIT_OF: &str = "SELECT action || ' ' || entity || ' ' || actor FROM audit_log \
  WHERE tenant = $1 ORDER BY seq";

/// An ingested document of `tenant` titled `title`, without the AI stages.
async fn document(h: &Harness, tenant: &str, title: &str) -> i64 {
  let tenancy = Tenancy::new(tenant);
  let create = domain::create_document(
    &h.app.db,
    &tenancy,
    NewDocument {
      source_id: h.sources[tenant],
      title: title.to_owned(),
      body: format!("The text of {title}."),
    },
  );
  run(tenancy.scope(create)).await.expect("document").id
}

// ---- the tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_admin_crud_and_audit() {
  let Some(h) = Harness::start("pipeline_admin_crud_and_audit", |app| app).await else {
    return;
  };
  let mut ada = Browser::new(&h, "ada", Some("acme"), &[ROLE_ADMIN]).await;

  // The list shows acme's seeded labels.
  let list = ada.get("/admin/acme/label").await;
  list.assert_status(StatusCode::OK);
  assert!(list.text().contains("invoice"), "{}", list.text());

  // Create, update, delete: each answered with a redirect, each in the audit log.
  let created = ada
    .post(
      "/admin/acme/label/@new",
      &[("name", "urgent"), ("description", "Needs attention")],
    )
    .await;
  assert!(created.status().is_redirection(), "{}", created.text());
  let found = ids(
    &h.main,
    "SELECT id FROM labels WHERE tenant = 'acme' AND name = 'urgent'",
    vec![],
  )
  .await;
  let id = found[0];
  ada.get(&format!("/admin/acme/label/{id}/@edit")).await;
  let updated = ada
    .post(
      &format!("/admin/acme/label/{id}/@edit"),
      &[("name", "urgent"), ("description", "Answer today")],
    )
    .await;
  assert!(updated.status().is_redirection(), "{}", updated.text());
  let description = texts(
    &h.main,
    "SELECT description FROM labels WHERE id = $1",
    params![id],
  )
  .await;
  assert_eq!(description, ["Answer today"]);
  let deleted = ada
    .post(&format!("/admin/acme/label/{id}/@delete"), &[])
    .await;
  assert!(deleted.status().is_redirection(), "{}", deleted.text());
  assert!(
    ids(&h.main, "SELECT id FROM labels WHERE id = $1", params![id])
      .await
      .is_empty()
  );
  let audit = texts(&h.main, AUDIT_OF, params!["acme"]).await;
  assert_eq!(
    audit,
    [
      format!("admin.create label:{id} ada"),
      format!("admin.update label:{id} ada"),
      format!("admin.delete label:{id} ada"),
    ]
  );

  // globex's admin writes to globex's own database, and its audit chain is its own.
  let mut gil = Browser::new(&h, "gil", Some("globex"), &[ROLE_ADMIN]).await;
  gil
    .get("/admin/globex/label")
    .await
    .assert_status(StatusCode::OK);
  let created = gil
    .post(
      "/admin/globex/label/@new",
      &[("name", "dringend"), ("description", "Sofort")],
    )
    .await;
  assert!(created.status().is_redirection(), "{}", created.text());
  assert_eq!(
    texts(
      &h.globex,
      "SELECT name FROM labels WHERE name = 'dringend'",
      vec![]
    )
    .await,
    ["dringend"]
  );
  assert!(
    texts(
      &h.main,
      "SELECT name FROM labels WHERE name = 'dringend'",
      vec![]
    )
    .await
    .is_empty()
  );
  let audit = texts(&h.globex, AUDIT_OF, params!["globex"]).await;
  assert_eq!(audit.len(), 1);
  assert!(audit[0].starts_with("admin.create label:"), "{audit:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_admin_denied_without_role() {
  let Some(h) = Harness::start("pipeline_admin_denied_without_role", |app| app).await else {
    return;
  };
  // Nobody signed in: 401, before any page renders.
  let mut anonymous = Browser::new(&h, "nobody", None, &[]).await;
  anonymous
    .get("/admin/acme/label")
    .await
    .assert_status(StatusCode::UNAUTHORIZED);
  // An editor, a reviewer of another tenant, an admin without a tenant: 403.
  for (sub, tenant, role) in [
    ("ed", Some("acme"), ROLE_EDITOR),
    ("gil", Some("globex"), ROLE_REVIEWER),
    ("ada", None, ROLE_ADMIN),
  ] {
    let mut who = Browser::new(&h, sub, tenant, &[role]).await;
    let res = who.get("/admin/acme/label").await;
    res.assert_status(StatusCode::FORBIDDEN);
    assert!(!res.text().contains("invoice"), "{}", res.text());
  }
  // A reviewer reads labels but may not create, edit or delete one, nor delete a document.
  let mut rita = Browser::new(&h, "rita", Some("acme"), &[ROLE_REVIEWER]).await;
  rita
    .get("/admin/acme/label")
    .await
    .assert_status(StatusCode::OK);
  let res = rita
    .post(
      "/admin/acme/label/@new",
      &[("name", "sneaky"), ("description", "x")],
    )
    .await;
  res.assert_status(StatusCode::FORBIDDEN);
  let label = ids(
    &h.main,
    "SELECT id FROM labels WHERE tenant = 'acme' AND name = 'spam'",
    vec![],
  )
  .await[0];
  rita
    .post(&format!("/admin/acme/label/{label}/@delete"), &[])
    .await
    .assert_status(StatusCode::FORBIDDEN);
  let doc = document(&h, "acme", "Kept").await;
  rita
    .post(&format!("/admin/acme/document/{doc}/@delete"), &[])
    .await
    .assert_status(StatusCode::FORBIDDEN);
  // Documents are never created in the admin, whatever the role.
  let mut ada = Browser::new(&h, "ada", Some("acme"), &[ROLE_ADMIN]).await;
  let res = ada.get("/admin/acme/document/@new").await;
  assert!(
    matches!(res.status(), StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
    "{}",
    res.status()
  );
  assert_eq!(
    ids(
      &h.main,
      "SELECT id FROM labels WHERE name = 'sneaky'",
      vec![]
    )
    .await
    .len(),
    0
  );
  assert!(texts(&h.main, AUDIT_OF, params!["acme"]).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_admin_bulk_rerun_extraction() {
  let Some(h) = Harness::start("pipeline_admin_bulk_rerun_extraction", |app| app).await else {
    return;
  };
  let title = "Acme contract";
  // Two runs through the AI stages: the first, then the re-run.
  let script = id_effect_kit::ai::testing::ScriptedModel::new()
    .reply(extraction(title, "en", Some("Acme Corp")))
    .tool_call("c1", "list_labels", serde_json::json!({}))
    .reply(serde_json::json!({ "labels": ["contract"] }).to_string())
    .reply(extraction(title, "en", Some("Acme Corporation")))
    .tool_call("c2", "list_labels", serde_json::json!({}))
    .reply(serde_json::json!({ "labels": ["contract", "invoice"] }).to_string());
  h.scripts.add(title, script);
  let acme = Tenancy::new("acme");
  let doc = run(stages::ingest(
    &h.app,
    &acme,
    NewDocument {
      source_id: h.sources["acme"],
      title: title.to_owned(),
      body: "Both parties agree.".to_owned(),
    },
  ))
  .await
  .expect("ingest")
  .id;
  h.drain().await;
  assert_eq!(h.status("acme", doc).await, "review");

  let mut rita = Browser::new(&h, "rita", Some("acme"), &[ROLE_REVIEWER]).await;
  let list = rita.get("/admin/acme/document").await;
  list.assert_status(StatusCode::OK);
  assert!(list.text().contains("rerun_extraction"), "{}", list.text());
  let id = doc.to_string();
  let res = rita
    .post(
      "/admin/acme/document/@bulk",
      &[("action", "rerun_extraction"), ("id", &id)],
    )
    .await;
  assert!(res.status().is_redirection(), "{}", res.text());
  assert_eq!(h.status("acme", doc).await, "ingested");
  let audit = texts(&h.main, AUDIT_OF, params!["acme"]).await;
  assert_eq!(audit, ["admin.bulk.rerun_extraction document rita"]);

  // The jobs wait for the action to commit, then the stages run again.
  tokio::time::sleep(RERUN_DELAY + Duration::from_millis(500)).await;
  h.drain().await;
  assert_eq!(h.status("acme", doc).await, "review");
  let extractions = texts(
    &h.main,
    "SELECT organization FROM extractions WHERE document_id = $1 ORDER BY id",
    params![doc],
  )
  .await;
  assert_eq!(extractions, ["Acme Corp", "Acme Corporation"]);
  let labels = texts(
    &h.main,
    "SELECT l.name FROM labels l JOIN document_labels dl ON dl.label_id = l.id \
     WHERE dl.document_id = $1 ORDER BY l.name",
    params![doc],
  )
  .await;
  assert_eq!(labels, ["contract", "invoice"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_admin_label_picker_tenant_scoped() {
  let Some(h) = Harness::start("pipeline_admin_label_picker_tenant_scoped", |app| app).await else {
    return;
  };
  let doc = document(&h, "acme", "Picked").await;
  // A label of another tenant in acme's (shared) database.
  let stray = ids(
    &h.main,
    "INSERT INTO labels (tenant, name, description) VALUES ('globex', 'secret', 'x') RETURNING id",
    vec![],
  )
  .await[0];
  let contract = ids(
    &h.main,
    "SELECT id FROM labels WHERE tenant = 'acme' AND name = 'contract'",
    vec![],
  )
  .await[0];

  let mut rita = Browser::new(&h, "rita", Some("acme"), &[ROLE_REVIEWER]).await;
  let detail = rita.get(&format!("/admin/acme/document/{doc}")).await;
  detail.assert_status(StatusCode::OK);
  // The picker offers acme's labels only.
  let picker = rita.get("/admin/acme/document/@pick/labels?_q=").await;
  picker.assert_status(StatusCode::OK);
  let options = picker.text();
  for name in ["invoice", "contract", "spam"] {
    assert!(options.contains(name), "{options}");
  }
  assert!(!options.contains("secret"), "{options}");
  let searched = rita.get("/admin/acme/document/@pick/labels?_q=secr").await;
  assert!(!searched.text().contains("secret"), "{}", searched.text());

  // Adding another tenant's label is refused; adding acme's own is audited.
  let rel = format!("/admin/acme/document/{doc}/@rel/labels");
  let refused = rita
    .post(&rel, &[("op", "add"), ("target", &stray.to_string())])
    .await;
  refused.assert_status(StatusCode::BAD_REQUEST);
  let added = rita
    .post(&rel, &[("op", "add"), ("target", &contract.to_string())])
    .await;
  assert!(added.status().is_redirection(), "{}", added.text());
  let linked = ids(
    &h.main,
    "SELECT label_id FROM document_labels WHERE document_id = $1",
    params![doc],
  )
  .await;
  assert_eq!(linked, [contract]);
  let audit = texts(&h.main, AUDIT_OF, params!["acme"]).await;
  assert_eq!(audit, [format!("admin.relation.add document:{doc} rita")]);
  let detail = rita.get(&format!("/admin/acme/document/{doc}")).await;
  assert!(detail.text().contains("contract"), "{}", detail.text());

  // globex's reviewer cannot reach acme's picker at all.
  let mut gil = Browser::new(&h, "gil", Some("globex"), &[ROLE_REVIEWER]).await;
  gil
    .get("/admin/acme/document/@pick/labels?_q=")
    .await
    .assert_status(StatusCode::FORBIDDEN);
}
