//! A templated message sent through `SmtpMailer` to a real SMTP server, the `id_effect_test`
//! [`Mailpit`] container (pinned by digest), and read back from Mailpit's HTTP API: envelope,
//! subject and both rendered bodies (msg.email#1 #2 #3).
//!
//! Without Docker the test prints `SKIP` and passes, unless `ID_EFFECT_REQUIRE_DOCKER=1` (the CI
//! `containers-integration` job), where a missing Docker daemon fails it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Exit, HNil};
use id_effect_mail::{
  Address, MailError, Mailbox, MailerEnv, SmtpConfig, SmtpMailer, Templates, deliver,
};
use id_effect_test::{ContainerError, ContainerImage, Mailpit, MailpitContainer, require_docker};
use id_effect_tokio::TokioRuntime;
use serde_json::{Value, json};

async fn run<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + std::fmt::Debug + 'static,
{
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// `GET {api}{path}` as JSON.
async fn api(client: &reqwest::Client, mailpit: &MailpitContainer, path: &str) -> Value {
  let url = format!("{}{path}", mailpit.api_url());
  let response = client.get(&url).send().await.expect(&url);
  assert!(
    response.status().is_success(),
    "GET {url}: {}",
    response.status()
  );
  serde_json::from_str(&response.text().await.unwrap()).expect("JSON")
}

async fn checks(mailpit: MailpitContainer) {
  let templates =
    Templates::from_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/templates")).unwrap();
  let message = templates
    .message(
      "welcome",
      &json!({
        "user": { "name": "Carol & Co" },
        "confirm_url": "https://ledger.example/confirm",
        "company": "Ledger GmbH",
      }),
    )
    .unwrap()
    .from(Mailbox::named(
      "Ledger",
      Address::parse("hello@ledger.example").unwrap(),
    ))
    .to(Address::parse("carol@ledger.example").unwrap())
    .build()
    .unwrap();
  let config = SmtpConfig::plaintext(mailpit.host(), mailpit.smtp_port());
  let sent =
    run(deliver::<MailerEnv, _>(message).provide_layer(SmtpMailer::layer::<MailError>(config)))
      .await
      .expect("Mailpit accepts the message");
  let message_id = sent.id.expect("a Message-ID");

  let client = reqwest::Client::new();
  let deadline = Instant::now() + Duration::from_secs(10);
  let listed = loop {
    let list = api(&client, &mailpit, "/api/v1/messages").await;
    if list["total"].as_u64() == Some(1) {
      break list;
    }
    assert!(Instant::now() < deadline, "no message in Mailpit: {list}");
    tokio::time::sleep(Duration::from_millis(100)).await;
  };
  let summary = &listed["messages"][0];
  assert_eq!(summary["Subject"], "Welcome to Ledger, Carol & Co");
  assert_eq!(
    format!("<{}>", summary["MessageID"].as_str().unwrap()),
    message_id
  );

  let id = summary["ID"].as_str().unwrap();
  let full = api(&client, &mailpit, &format!("/api/v1/message/{id}")).await;
  assert_eq!(full["From"]["Address"], "hello@ledger.example");
  assert_eq!(full["From"]["Name"], "Ledger");
  assert_eq!(full["To"][0]["Address"], "carol@ledger.example");
  let html = full["HTML"].as_str().unwrap();
  assert!(html.contains("<h1>Hello Carol &amp; Co</h1>"), "{html}");
  assert!(html.contains("Ledger &middot; Ledger GmbH"), "{html}");
  let text = full["Text"].as_str().unwrap();
  assert!(text.starts_with("Hello Carol & Co,"), "{text}");
  assert!(text.contains("-- Ledger, Ledger GmbH"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn smtp_delivery_reaches_mailpit() {
  if !require_docker() {
    return;
  }
  let mailpit = run(Mailpit::new().start::<ContainerError, HNil>())
    .await
    .expect("the Mailpit container starts");
  // The checks run in their own task so the container is removed even when one fails.
  let outcome = tokio::spawn(checks(mailpit.clone())).await;
  run(mailpit.container().remove::<ContainerError, HNil>())
    .await
    .expect("the container is removed");
  if let Err(e) = outcome {
    std::panic::resume_unwind(e.into_panic());
  }
}
