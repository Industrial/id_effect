//! The htmx + maud review queue on PostgreSQL (release spec 0.13.0, D1 stage 5;
//! r13-w7-pipeline-backoffice): pages and fragments, the partial updates htmx asks for, the
//! accept form and CSRF on every post. HTML is read through the in-process HTTP client
//! (`id_effect_test`) with the app's whole router; no browser.

#[path = "../fixtures/support.rs"]
mod support;

use id_effect_kit::sql::{SqlClient, SqlValue, params};
use id_effect_kit::tenancy::Tenancy;
use id_effect_kit::test::http::{StatusCode, TestClient, TestResponse};
use pipeline::domain::NewDocument;
use pipeline::mcp::McpSettings;
use pipeline::stages;
use pipeline::tenancy::ROLE_REVIEWER;
use support::{Harness, run, well_behaved};

// ---- a browser session over the in-process client ----

/// One signed-in user: a bearer token, the CSRF cookie and the masked token of the last page.
struct Browser {
  client: TestClient<axum::Router>,
  token: String,
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

/// The masked CSRF token of the layout's `hx-headers` (what htmx sends on every request).
fn hx_token(body: &str) -> Option<String> {
  let marker = "x-csrf-token&quot;:&quot;";
  let start = body.find(marker)? + marker.len();
  let len = body[start..].find("&quot;")?;
  Some(body[start..start + len].to_owned())
}

impl Browser {
  async fn new(h: &Harness, sub: &str, tenant: &str) -> Self {
    let router = pipeline::router(&h.app, &h.keys, &McpSettings::default()).expect("router");
    Self {
      client: TestClient::new(router),
      token: h.token(sub, Some(tenant), &[ROLE_REVIEWER]).await,
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
    if let Some(token) = hx_token(&res.text()) {
      self.csrf = Some(token);
    }
  }

  /// A request with `headers`; a form `post` carries the cookie and the token in the
  /// `x-csrf-token` header, as htmx sends it.
  async fn send(
    &mut self,
    post: Option<&[(&str, &str)]>,
    uri: &str,
    headers: &[(&str, &str)],
  ) -> TestResponse {
    let mut req = match post {
      Some(form) => {
        let body: Vec<String> = form
          .iter()
          .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
          .collect();
        self
          .client
          .post(uri)
          .body(body.join("&"), "application/x-www-form-urlencoded")
      }
      None => self.client.get(uri),
    }
    .bearer(&self.token);
    if let Some(cookie) = &self.cookie {
      req = req.header("cookie", cookie);
    }
    if post.is_some()
      && let Some(csrf) = &self.csrf
    {
      req = req.header("x-csrf-token", csrf);
    }
    for (name, value) in headers {
      req = req.header(name, value);
    }
    let res = run(req.send()).await.expect("the request is answered");
    self.remember(&res);
    res
  }
}

const HTMX_DETAIL: [(&str, &str); 2] = [("hx-request", "true"), ("hx-target", "review-detail")];

async fn status_and_title(db: &std::sync::Arc<dyn SqlClient>, id: i64) -> (String, String) {
  let rows = run(db.query(
    "SELECT status, title FROM documents WHERE id = $1",
    &params![id],
  ))
  .await
  .expect("document");
  (
    rows[0].get::<String>("status").expect("status"),
    rows[0].get::<String>("title").expect("title"),
  )
}

/// A document of acme in review, through the stages.
async fn in_review(h: &Harness, title: &str) -> i64 {
  h.scripts
    .add(title, well_behaved(title, "en", &["invoice"]));
  let acme = Tenancy::new("acme");
  let doc = run(stages::ingest(
    &h.app,
    &acme,
    NewDocument {
      source_id: h.sources["acme"],
      title: title.to_owned(),
      body: format!("The text of {title}."),
    },
  ))
  .await
  .expect("ingest")
  .id;
  h.drain().await;
  assert_eq!(h.status("acme", doc).await, "review");
  doc
}

async fn published(h: &Harness) -> i64 {
  let none: Vec<SqlValue> = vec![];
  Harness::count(
    &h.main,
    "SELECT count(*) FROM documents WHERE status = 'published'",
    none,
  )
  .await
}

// ---- the tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_htmx_post_requires_csrf() {
  let Some(h) = Harness::start("pipeline_htmx_post_requires_csrf", |app| app).await else {
    return;
  };
  let doc = in_review(&h, "Acme invoice").await;
  let path = format!("/review/{doc}");
  let mut rita = Browser::new(&h, "rita", "acme").await;
  let page = rita.send(None, "/review", &[]).await;
  page.assert_status(StatusCode::OK);
  assert!(rita.cookie.is_some(), "the session's CSRF cookie is set");
  assert!(
    hx_token(&page.text()).is_some(),
    "the layout sends the token with htmx"
  );
  let version = run(
    h.main
      .query("SELECT version FROM documents WHERE id = $1", &params![doc]),
  )
  .await
  .expect("version")[0]
    .get::<i64>("version")
    .expect("version")
    .to_string();
  let accept = [("title", "Acme invoice"), ("version", version.as_str())];

  // Without the token, without the cookie, or with a forged token: refused before the handler.
  let token = rita.csrf.take();
  rita
    .send(Some(&accept), &path, &HTMX_DETAIL)
    .await
    .assert_status(StatusCode::FORBIDDEN);
  let cookie = rita.cookie.take();
  rita.csrf.clone_from(&token);
  rita
    .send(Some(&accept), &path, &HTMX_DETAIL)
    .await
    .assert_status(StatusCode::FORBIDDEN);
  rita.cookie.clone_from(&cookie);
  rita.csrf = Some("A".repeat(86));
  rita
    .send(Some(&accept), &path, &HTMX_DETAIL)
    .await
    .assert_status(StatusCode::FORBIDDEN);
  rita.csrf = Some("not even base64!".to_owned());
  rita
    .send(Some(&accept), &path, &HTMX_DETAIL)
    .await
    .assert_status(StatusCode::FORBIDDEN);
  assert_eq!(status_and_title(&h.main, doc).await.0, "review");
  // The admin's posts need it too.
  rita.csrf = None;
  let bulk = rita
    .send(
      Some(&[("action", "rerun_extraction"), ("id", &doc.to_string())]),
      "/admin/acme/document/@bulk",
      &[],
    )
    .await;
  bulk.assert_status(StatusCode::FORBIDDEN);
  assert_eq!(status_and_title(&h.main, doc).await.0, "review");

  // With the masked token htmx sends (`x-csrf-token`), the post goes through.
  rita.csrf = token;
  let ok = rita.send(Some(&accept), &path, &HTMX_DETAIL).await;
  ok.assert_status(StatusCode::OK);
  assert_eq!(status_and_title(&h.main, doc).await.0, "published");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_review_queue_publishes_with_partial_updates() {
  let Some(h) = Harness::start(
    "pipeline_review_queue_publishes_with_partial_updates",
    |app| app,
  )
  .await
  else {
    return;
  };
  let first = in_review(&h, "Acme invoice").await;
  let second = in_review(&h, "Acme <script>alert(1)</script> memo").await;
  let mut rita = Browser::new(&h, "rita", "acme").await;

  // The page: layout, the queue table and an empty document panel; titles escaped.
  let page = rita.send(None, "/review", &[]).await;
  page.assert_status(StatusCode::OK);
  // Message arguments are bidi-isolated (U+2068 … U+2069).
  let html = page.text().replace(['\u{2068}', '\u{2069}'], "");
  assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
  assert!(html.contains(r#"id="review-queue""#), "{html}");
  assert!(html.contains(r#"id="review-detail""#), "{html}");
  assert!(html.contains("2 documents wait for review."), "{html}");
  assert!(html.contains("&lt;script&gt;"), "{html}");
  assert!(!html.contains("<script>alert"), "{html}");
  assert!(html.contains(id_effect_kit::ui::htmx::HTMX_PATH), "{html}");

  // htmx asks for the queue region alone, or a document's panel alone.
  let queue = rita
    .send(
      None,
      "/review",
      &[("hx-request", "true"), ("hx-target", "review-queue")],
    )
    .await;
  assert!(
    queue.text().starts_with(r#"<div id="review-queue""#),
    "{}",
    queue.text()
  );
  let panel = rita
    .send(None, &format!("/review/{first}"), &HTMX_DETAIL)
    .await;
  panel.assert_status(StatusCode::OK);
  let fragment = panel.text();
  assert!(
    fragment.starts_with(r#"<section class="panel" id="review-detail""#),
    "{fragment}"
  );
  assert!(fragment.contains("Acme Corp"), "the extraction: {fragment}");
  assert!(
    fragment.contains("<li>invoice</li>"),
    "the labels: {fragment}"
  );
  assert!(fragment.contains(r#"name="version""#), "{fragment}");
  // The same URL without htmx is the whole page (a reload, a shared link).
  let whole = rita.send(None, &format!("/review/{first}"), &[]).await;
  assert!(whole.text().starts_with("<!DOCTYPE html>"));

  // Accept with a corrected title: the panel says so, the queue is told to reload.
  let version = |d: i64| {
    let db = std::sync::Arc::clone(&h.main);
    async move {
      run(db.query("SELECT version FROM documents WHERE id = $1", &params![d]))
        .await
        .expect("version")[0]
        .get::<i64>("version")
        .expect("version")
        .to_string()
    }
  };
  let v1 = version(first).await;
  let accepted = rita
    .send(
      Some(&[("title", "Acme invoice (Q3)"), ("version", &v1)]),
      &format!("/review/{first}"),
      &HTMX_DETAIL,
    )
    .await;
  accepted.assert_status(StatusCode::OK);
  assert_eq!(accepted.header("hx-trigger"), Some("queue-changed"));
  assert!(
    accepted.text().contains("is published"),
    "{}",
    accepted.text()
  );
  assert_eq!(
    status_and_title(&h.main, first).await,
    ("published".to_owned(), "Acme invoice (Q3)".to_owned())
  );
  assert_eq!(published(&h).await, 1);
  // Accepting it again: no longer in review.
  let again = rita
    .send(
      Some(&[("title", "Acme invoice (Q3)"), ("version", &v1)]),
      &format!("/review/{first}"),
      &HTMX_DETAIL,
    )
    .await;
  again.assert_status(StatusCode::CONFLICT);
  assert!(
    again.text().contains("no longer waiting for review"),
    "{}",
    again.text()
  );
  let queue = rita
    .send(
      None,
      "/review",
      &[("hx-request", "true"), ("hx-target", "review-queue")],
    )
    .await;
  assert!(
    queue.text().contains("One document waits for review."),
    "{}",
    queue.text()
  );

  // A stale version is a conflict; a plain form post (no htmx) redirects back to the queue.
  let stale = rita
    .send(
      Some(&[("title", "Memo"), ("version", "999")]),
      &format!("/review/{second}"),
      &HTMX_DETAIL,
    )
    .await;
  stale.assert_status(StatusCode::CONFLICT);
  assert!(
    stale.text().contains("Someone else changed"),
    "{}",
    stale.text()
  );
  let v2 = version(second).await;
  let plain = rita
    .send(
      Some(&[("title", "Memo"), ("version", &v2)]),
      &format!("/review/{second}"),
      &[],
    )
    .await;
  plain.assert_status(StatusCode::SEE_OTHER);
  assert_eq!(plain.header("location"), Some("/review"));
  let empty = rita.send(None, "/review", &[]).await;
  assert!(empty.text().contains("No document is waiting for review."));

  // Another tenant's reviewer finds nothing of acme's; a missing id is 404, a bad version 400.
  let mut gil = Browser::new(&h, "gil", "globex").await;
  gil
    .send(None, &format!("/review/{second}"), &[])
    .await
    .assert_status(StatusCode::NOT_FOUND);
  let gil_page = gil.send(None, "/review", &[]).await;
  assert!(!gil_page.text().contains("Memo"), "{}", gil_page.text());
  rita
    .send(None, "/review/999999", &HTMX_DETAIL)
    .await
    .assert_status(StatusCode::NOT_FOUND);
  let third = in_review(&h, "Acme order").await;
  rita.send(None, "/review", &[]).await;
  rita
    .send(
      Some(&[("title", "Order"), ("version", "first")]),
      &format!("/review/{third}"),
      &HTMX_DETAIL,
    )
    .await
    .assert_status(StatusCode::BAD_REQUEST);
}
