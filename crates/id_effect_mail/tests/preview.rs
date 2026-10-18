//! `MailPreviews` through the in-process HTTP client: the index at `/_mail/previews`, each
//! preview's page and its HTML, text and raw MIME views, render failures, and templates
//! re-read from disk between requests (msg.email#3).

#![cfg(feature = "preview")]

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use id_effect::Exit;
use id_effect_mail::{MailPreviews, PREVIEWS_PATH, Templates};
use id_effect_test::http::{StatusCode, TestClient, TestResponse};
use serde_json::json;

fn templates_dir() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/templates")
}

fn welcome_fixture() -> serde_json::Value {
  json!({
    "user": { "name": "Carol" },
    "confirm_url": "https://ledger.example/confirm",
    "company": "Ledger GmbH",
  })
}

fn get(client: &TestClient<axum::Router>, path: &str) -> TestResponse {
  match client.get(path).send().block_on() {
    Exit::Success(response) => response,
    Exit::Failure(cause) => panic!("GET {path}: {cause:?}"),
  }
}

#[test]
fn preview_index_lists_registered_mails() {
  let previews = MailPreviews::new(Templates::from_dir(templates_dir()).unwrap())
    .preview("welcome", "welcome", welcome_fixture())
    .preview(
      "receipt-paid",
      "receipt",
      json!({ "number": 7, "amount": "12.00 EUR", "company": "Ledger GmbH" }),
    )
    .preview(
      "welcome-broken",
      "welcome",
      json!({ "user": { "name": "x" } }),
    );
  assert_eq!(
    previews.names().collect::<Vec<_>>(),
    ["welcome", "receipt-paid", "welcome-broken"]
  );
  let client = TestClient::new(previews.router::<()>());

  let index = get(&client, PREVIEWS_PATH);
  index.assert_status(StatusCode::OK);
  assert!(
    index
      .header("content-type")
      .unwrap()
      .starts_with("text/html")
  );
  let body = index.text();
  for link in [
    "/_mail/previews/welcome\"",
    "/_mail/previews/receipt-paid\"",
    "/_mail/previews/welcome/html\"",
    "/_mail/previews/receipt-paid/raw\"",
  ] {
    assert!(body.contains(link), "{link} in {body}");
  }

  let page = get(&client, "/_mail/previews/welcome");
  page.assert_status(StatusCode::OK);
  let page = page.text();
  assert!(page.contains("<h1>Welcome to Ledger, Carol</h1>"), "{page}");
  assert!(
    page.contains("<iframe src=\"/_mail/previews/welcome/html\""),
    "{page}"
  );
  assert!(page.contains("recipient@example.com"), "{page}");

  let html = get(&client, "/_mail/previews/welcome/html");
  html.assert_status(StatusCode::OK);
  assert!(html.text().contains("<h1>Hello Carol</h1>"));

  let text = get(&client, "/_mail/previews/receipt-paid/text");
  text.assert_status(StatusCode::OK);
  assert!(
    text
      .header("content-type")
      .unwrap()
      .starts_with("text/plain")
  );
  assert!(text.text().starts_with("Paid 12.00 EUR for invoice 7."));

  let raw = get(&client, "/_mail/previews/welcome/raw").text();
  assert!(
    raw.contains("Subject: Welcome to Ledger, Carol\r\n"),
    "{raw}"
  );
  assert!(raw.contains("multipart/alternative"), "{raw}");
  assert!(raw.contains("From: previews@example.com\r\n"), "{raw}");

  get(&client, "/_mail/previews/receipt-paid/html").assert_status(StatusCode::NOT_FOUND);
  get(&client, "/_mail/previews/nope").assert_status(StatusCode::NOT_FOUND);
  get(&client, "/_mail/previews/welcome/pdf").assert_status(StatusCode::NOT_FOUND);

  let broken = get(&client, "/_mail/previews/welcome-broken");
  broken.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
  assert!(broken.text().contains("undefined"), "{}", broken.text());
}

#[test]
fn previews_reread_templates_from_disk() {
  let nanos = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_nanos();
  let dir = std::env::temp_dir().join(format!(
    "id-effect-mail-preview-{}-{nanos}",
    std::process::id()
  ));
  std::fs::create_dir_all(&dir).unwrap();
  let note = dir.join("note.txt");
  std::fs::write(&note, "v1 {{ who }}").unwrap();

  let templates = Templates::from_dir(&dir).unwrap().reload(true);
  let client = TestClient::new(
    MailPreviews::new(templates)
      .preview("note", "note", json!({ "who": "Carol" }))
      .router::<()>(),
  );
  assert_eq!(get(&client, "/_mail/previews/note/text").text(), "v1 Carol");
  std::fs::write(&note, "v2 {{ who }}").unwrap();
  assert_eq!(get(&client, "/_mail/previews/note/text").text(), "v2 Carol");
  std::fs::remove_dir_all(&dir).unwrap();
}
