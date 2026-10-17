//! `SendGridMailer` against a wiremock SendGrid: the v3 request shape, error mapping, and that
//! the default HTTP client does not retry the (non-idempotent) POST.

use id_effect::{Cause, Effect, Exit};
use id_effect_mail::{
  Address, Attachment, InlinePart, MailError, Mailbox, MailerEnv, Message, SendGridConfig,
  SendGridMailer, Sent, deliver,
};
use id_effect_platform::http::ReqwestHttpClientProvider;
use id_effect_tokio::TokioRuntime;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "SG.test-key";

fn message() -> Message {
  Message::builder()
    .from(Mailbox::named(
      "Ledger",
      Address::parse("billing@ledger.example").unwrap(),
    ))
    .to(Address::parse("carol@ledger.example").unwrap())
    .bcc(Address::parse("audit@ledger.example").unwrap())
    .subject("Invoice 42")
    .text("Your invoice is attached.")
    .html("<img src=\"cid:logo\"> Your invoice is attached.")
    .inline(InlinePart::new("logo", "image/png", b"png".to_vec()).with_filename("logo.png"))
    .attach(Attachment::new(
      "invoice.pdf",
      "application/pdf",
      b"%PDF".to_vec(),
    ))
    .build()
    .unwrap()
}

/// Sends `message()` through the reqwest-backed client (default timeouts and retry policy).
fn send(server: &MockServer) -> Effect<Sent, MailError> {
  let config = SendGridConfig::new(KEY).base_url(server.uri());
  deliver::<MailerEnv, _>(message())
    .provide_layer(ReqwestHttpClientProvider::layer().provide_to(SendGridMailer::layer(config)))
}

async fn run<A: Send + 'static>(eff: Effect<A, MailError>) -> Result<A, MailError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

#[tokio::test]
async fn posts_the_v3_body_with_the_bearer_key() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v3/mail/send"))
    .and(header("authorization", format!("Bearer {KEY}").as_str()))
    .and(header("content-type", "application/json"))
    .and(body_partial_json(json!({
      "personalizations": [{
        "to": [{ "email": "carol@ledger.example" }],
        "bcc": [{ "email": "audit@ledger.example" }],
      }],
      "from": { "email": "billing@ledger.example", "name": "Ledger" },
      "subject": "Invoice 42",
      "content": [
        { "type": "text/plain", "value": "Your invoice is attached." },
        { "type": "text/html", "value": "<img src=\"cid:logo\"> Your invoice is attached." },
      ],
      "attachments": [
        { "content": "JVBERg==", "type": "application/pdf", "filename": "invoice.pdf",
          "disposition": "attachment" },
        { "content": "cG5n", "type": "image/png", "filename": "logo.png",
          "disposition": "inline", "content_id": "logo" },
      ],
    })))
    .respond_with(ResponseTemplate::new(202).insert_header("X-Message-Id", "sg-123"))
    .expect(1)
    .mount(&server)
    .await;
  let sent = run(send(&server)).await.expect("accepted");
  assert_eq!(sent.id.as_deref(), Some("sg-123"));
}

#[tokio::test]
async fn a_validation_error_is_a_rejection_with_the_reasons() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(400).set_body_json(json!({
      "errors": [{ "message": "The from address does not match a verified Sender Identity.",
                   "field": "from", "help": null }]
    })))
    .mount(&server)
    .await;
  match run(send(&server)).await {
    Err(MailError::Rejected { code: 400, message }) => {
      assert_eq!(
        message,
        "from: The from address does not match a verified Sender Identity."
      );
    }
    other => panic!("expected a 400 rejection, got {other:?}"),
  }
}

#[tokio::test]
async fn a_refused_key_is_unauthorized() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(401).set_body_json(json!({
      "errors": [{ "message": "The provided authorization grant is invalid" }]
    })))
    .mount(&server)
    .await;
  match run(send(&server)).await {
    Err(MailError::Unauthorized(m)) => assert!(m.contains("authorization grant")),
    other => panic!("expected unauthorized, got {other:?}"),
  }
}

#[tokio::test]
async fn an_outage_is_temporary_and_the_post_is_not_retried() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
    // The default policy retries idempotent methods only: one attempt, no duplicate send.
    .expect(1)
    .mount(&server)
    .await;
  match run(send(&server)).await {
    Err(e @ MailError::Temporary { code: 503, .. }) => assert!(e.is_transient()),
    other => panic!("expected a temporary failure, got {other:?}"),
  }
  server.verify().await;
}

#[tokio::test]
async fn rate_limiting_is_temporary() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(429))
    .expect(1)
    .mount(&server)
    .await;
  assert!(matches!(
    run(send(&server)).await,
    Err(MailError::Temporary { code: 429, .. })
  ));
}

#[tokio::test]
async fn an_unreachable_api_is_a_transport_error() {
  let config = SendGridConfig::new(KEY).base_url("http://127.0.0.1:1");
  let program = deliver::<MailerEnv, _>(message())
    .provide_layer(ReqwestHttpClientProvider::layer().provide_to(SendGridMailer::layer(config)));
  assert!(matches!(run(program).await, Err(MailError::Transport(_))));
}
