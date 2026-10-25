//! The back office in English and German on PostgreSQL (release spec 0.13.0, D1 stage 5;
//! r13-w7-pipeline-backoffice): `Accept-Language` negotiation, the per-user language override,
//! the admin's labels, and validation errors translated and bound to their fields. HTML is read
//! through the in-process HTTP client (`id_effect_test`) with the app's whole router.

#[path = "../fixtures/support.rs"]
mod support;

use id_effect_kit::sql::{SqlClient, params};
use id_effect_kit::tenancy::Tenancy;
use id_effect_kit::test::http::{StatusCode, TestClient, TestResponse};
use pipeline::domain::NewDocument;
use pipeline::mcp::McpSettings;
use pipeline::stages;
use pipeline::tenancy::ROLE_REVIEWER;
use support::{Harness, run, well_behaved};

// ---- a browser session over the in-process client ----

/// One signed-in reviewer: a bearer token, the CSRF cookie and the masked token of the last
/// page, and the `Accept-Language` its browser sends.
struct Browser {
  client: TestClient<axum::Router>,
  token: String,
  cookie: Option<String>,
  csrf: Option<String>,
  language: String,
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

/// `text` without the bidi isolation marks around message arguments.
fn plain(text: &str) -> String {
  text.replace(['\u{2068}', '\u{2069}'], "")
}

impl Browser {
  async fn new(h: &Harness, sub: &str, language: &str) -> Self {
    let router = pipeline::router(&h.app, &h.keys, &McpSettings::default()).expect("router");
    Self {
      client: TestClient::new(router),
      token: h.token(sub, Some("acme"), &[ROLE_REVIEWER]).await,
      cookie: None,
      csrf: None,
      language: language.to_owned(),
    }
  }

  async fn send(&mut self, post: Option<&[(&str, &str)]>, uri: &str, htmx: bool) -> TestResponse {
    let mut req = match post {
      Some(form) => {
        let mut body: Vec<String> = form
          .iter()
          .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
          .collect();
        if let Some(csrf) = &self.csrf {
          body.push(format!("csrf_token={}", encode(csrf)));
        }
        self
          .client
          .post(uri)
          .body(body.join("&"), "application/x-www-form-urlencoded")
      }
      None => self.client.get(uri),
    }
    .bearer(&self.token)
    .header("accept-language", &self.language);
    if let Some(cookie) = &self.cookie {
      req = req.header("cookie", cookie);
    }
    if htmx {
      req = req
        .header("hx-request", "true")
        .header("hx-target", "review-detail");
    }
    let res = run(req.send()).await.expect("the request is answered");
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
    res
  }

  async fn get(&mut self, uri: &str) -> TestResponse {
    self.send(None, uri, false).await
  }
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
  doc
}

async fn version(db: &std::sync::Arc<dyn SqlClient>, id: i64) -> String {
  run(db.query("SELECT version FROM documents WHERE id = $1", &params![id]))
    .await
    .expect("version")[0]
    .get::<i64>("version")
    .expect("version")
    .to_string()
}

// ---- the tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_ui_renders_de_and_en() {
  let Some(h) = Harness::start("pipeline_ui_renders_de_and_en", |app| app).await else {
    return;
  };
  let doc = in_review(&h, "Acme invoice").await;

  // German from Accept-Language: the queue, a document, the admin's labels.
  let mut rita = Browser::new(&h, "rita", "de-DE,de;q=0.9,en;q=0.5").await;
  let page = rita.get("/review").await;
  page.assert_status(StatusCode::OK);
  assert_eq!(page.header("content-language"), Some("de"));
  let html = plain(&page.text());
  assert!(html.contains(r#"<html lang="de">"#), "{html}");
  for text in [
    "Prüfwarteschlange",
    "Ein Dokument wartet auf Prüfung.",
    "Titel",
    "Quelle",
    "Acme invoice",
  ] {
    assert!(html.contains(text), "{text}: {html}");
  }
  let detail = plain(&rita.get(&format!("/review/{doc}")).await.text());
  for text in [
    "in Prüfung",
    "Zusammenfassung",
    "Organisation",
    "Annehmen und veröffentlichen",
  ] {
    assert!(detail.contains(text), "{text}: {detail}");
  }
  let admin = plain(&rita.get("/admin/acme/document").await.text());
  assert!(admin.contains("Dokumente"), "{admin}");
  assert!(admin.contains("Titel"), "{admin}");
  assert!(admin.contains("Extraktion erneut ausführen"), "{admin}");

  // English from Accept-Language, and as the fallback for a language the app does not speak.
  for language in ["en-GB,en;q=0.8", "fr-FR,fr;q=0.9"] {
    let mut rex = Browser::new(&h, "rex", language).await;
    let page = rex.get("/review").await;
    assert_eq!(page.header("content-language"), Some("en"));
    let html = plain(&page.text());
    assert!(html.contains(r#"<html lang="en">"#), "{html}");
    assert!(html.contains("One document waits for review."), "{html}");
    let detail = plain(&rex.get(&format!("/review/{doc}")).await.text());
    assert!(detail.contains("Accept and publish"), "{detail}");
    assert!(detail.contains("Summary"), "{detail}");
    let admin = plain(&rex.get("/admin/acme/document").await.text());
    assert!(admin.contains("Documents"), "{admin}");
  }

  // The per-user override beats the browser's language, for that user only, admin included.
  let mut rex = Browser::new(&h, "rex", "en").await;
  rex.get("/review").await;
  let switched = rex
    .send(Some(&[("locale", "de")]), "/review/locale", false)
    .await;
  switched.assert_status(StatusCode::SEE_OTHER);
  let page = plain(&rex.get("/review").await.text());
  assert!(page.contains("Prüfwarteschlange"), "{page}");
  let admin = plain(&rex.get("/admin/acme/document").await.text());
  assert!(admin.contains("Dokumente"), "{admin}");
  let mut ella = Browser::new(&h, "ella", "en").await;
  assert!(plain(&ella.get("/review").await.text()).contains("Review queue"));
  // Only the locales the app speaks can be chosen.
  let refused = rex
    .send(Some(&[("locale", "fr")]), "/review/locale", false)
    .await;
  refused.assert_status(StatusCode::BAD_REQUEST);
  assert!(plain(&refused.text()).contains("Diese Sprache ist nicht verfügbar."));
  let back = rex
    .send(Some(&[("locale", "en")]), "/review/locale", false)
    .await;
  back.assert_status(StatusCode::SEE_OTHER);
  assert!(plain(&rex.get("/review").await.text()).contains("Review queue"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_validation_errors_translated() {
  let Some(h) = Harness::start("pipeline_validation_errors_translated", |app| app).await else {
    return;
  };
  let doc = in_review(&h, "Acme invoice").await;
  let path = format!("/review/{doc}");
  let v = version(&h.main, doc).await;
  let long = "x".repeat(201);

  let mut rita = Browser::new(&h, "rita", "de").await;
  rita.get("/review").await;
  // An empty title: the form again (422), the error bound to the field, in German.
  let empty = rita
    .send(Some(&[("title", ""), ("version", &v)]), &path, true)
    .await;
  empty.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
  let html = plain(&empty.text());
  assert!(html.contains(r#"aria-invalid="true""#), "{html}");
  assert!(html.contains(r#"id="field-title-errors""#), "{html}");
  assert!(html.contains("Darf nicht leer sein."), "{html}");
  assert!(
    html.contains("Bitte korrigieren Sie die Fehler unten."),
    "{html}"
  );
  // Blank (spaces only) is empty too.
  let blank = rita
    .send(Some(&[("title", "   "), ("version", &v)]), &path, true)
    .await;
  blank.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
  assert!(plain(&blank.text()).contains("Darf nicht leer sein."));
  // Too long: the bound in the message, the submitted value kept.
  let too_long = rita
    .send(Some(&[("title", &long), ("version", &v)]), &path, true)
    .await;
  too_long.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
  let html = plain(&too_long.text());
  assert!(
    html.contains("Darf höchstens 200 Zeichen lang sein."),
    "{html}"
  );
  assert!(html.contains(&format!(r#"value="{long}""#)), "{html}");

  // The same errors in English.
  let mut rex = Browser::new(&h, "rex", "en").await;
  rex.get("/review").await;
  let empty = rex
    .send(Some(&[("title", ""), ("version", &v)]), &path, true)
    .await;
  empty.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
  let html = plain(&empty.text());
  assert!(html.contains(r#"id="field-title-errors""#), "{html}");
  assert!(html.contains("Please correct the errors below."), "{html}");
  assert!(!html.contains("Darf"), "{html}");

  // Nothing was published by an invalid form.
  assert_eq!(h.status("acme", doc).await, "review");
}
