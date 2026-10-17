//! Tracing and redaction (obs.tracing #6 #7, web.client #4 #6, obs.logging #7): the prod mail
//! transport, SendGrid over the traced HTTP client (a wiremock server here), sends the W3C
//! `traceparent` of its client span; every statement runs in a `db.query` client span; the
//! sampler comes from the environment; logs never show passwords, tokens or the profile's
//! secrets.

mod common;

use std::sync::Arc;

use common::{run, test_config, with_ledger};
use id_effect::Effect;
use id_effect_kit::logger::{LogLevel, LogRecord};
use id_effect_kit::mail::{Address, Message};
use id_effect_kit::otel::{
  OtelInMemoryExporters, OtelSampler, OtelSpanHook, install_w3c_propagators,
  sdk_tracer_provider_with_in_memory_exporter,
};
use ledger::config::MailTransport;
use ledger::{Database, mail, observability};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The attribute `key` of the finished span `name`, if both exist.
fn attribute(exporters: &OtelInMemoryExporters, name: &str, key: &str) -> Option<String> {
  let spans = exporters.spans.get_finished_spans().expect("spans");
  let span = spans.iter().find(|s| s.name == name)?;
  span
    .attributes
    .iter()
    .find(|kv| kv.key.as_str() == key)
    .map(|kv| kv.value.to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sendgrid_mail_goes_out_in_a_traced_client_span() {
  let sendgrid = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v3/mail/send"))
    .and(header_exists("traceparent"))
    .and(header_exists("authorization"))
    .respond_with(ResponseTemplate::new(202).insert_header("x-message-id", "sg-1"))
    .expect(1)
    .mount(&sendgrid)
    .await;
  let mut config = test_config().await.mail;
  config.transport = MailTransport::Sendgrid;
  config.sendgrid_api_key = Some(id_effect_kit::config::Secret::new("SG.test-key".to_owned()));
  config.sendgrid_base_url = Some(sendgrid.uri());
  let mailer = mail::mailer(&config, mail::http_client()).expect("the SendGrid transport");

  install_w3c_propagators();
  let exporters = OtelInMemoryExporters::default();
  let provider = sdk_tracer_provider_with_in_memory_exporter(&exporters.spans);
  let message = Message::builder()
    .from(mail::sender(&config).unwrap())
    .to(Address::parse("carol@ledger.example").unwrap())
    .subject("Your statement")
    .text("Attached.")
    .build()
    .unwrap();
  let sent = mailer
    .send(message)
    .with_span("statement.mail")
    .with_span_hook(OtelSpanHook::for_provider(&provider, "ledger").into_hook());
  run(sent).await;
  let _ = provider.force_flush();

  // The client span is a child of the caller's, and its context went out in `traceparent`.
  let spans = exporters.spans.get_finished_spans().unwrap();
  let client = spans
    .iter()
    .find(|s| s.name == "POST")
    .expect("a client span");
  let parent = spans
    .iter()
    .find(|s| s.name == "statement.mail")
    .expect("the caller's span");
  assert_eq!(client.parent_span_id, parent.span_context.span_id());
  let received = &sendgrid.received_requests().await.unwrap()[0];
  let traceparent = received
    .headers
    .get("traceparent")
    .unwrap()
    .to_str()
    .unwrap();
  assert!(
    traceparent.contains(&client.span_context.span_id().to_string()),
    "{traceparent}"
  );
  assert!(attribute(&exporters, "POST", "http.response.status_code").is_some_and(|s| s == "202"));
  // The URL is recorded, the key is not.
  assert!(!format!("{:?}", client.attributes).contains("SG.test-key"));
}

#[tokio::test]
async fn statements_run_in_db_client_spans_without_their_parameters() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let database = Database::new(Arc::clone(&l.db), vec![]);
      let exporters = OtelInMemoryExporters::default();
      let provider = sdk_tracer_provider_with_in_memory_exporter(&exporters.spans);
      let query: Effect<_, _> = database.primary.query(
        "SELECT count(*) AS n FROM users WHERE username = $1 AND role <> 'nobody'",
        &id_effect_kit::sql::params!["a secret username"],
      );
      run(
        query
          .with_span("report")
          .with_span_hook(OtelSpanHook::for_provider(&provider, "ledger").into_hook()),
      )
      .await;
      let _ = provider.force_flush();
      assert_eq!(
        attribute(&exporters, "db.query", "db.system").as_deref(),
        Some("postgresql")
      );
      let statement = attribute(&exporters, "db.query", "db.statement").expect("the statement");
      assert!(
        statement.contains("WHERE username = $1 AND role <> ?"),
        "{statement}"
      );
      let spans = format!("{:?}", exporters.spans.get_finished_spans().unwrap());
      assert!(
        !spans.contains("a secret username"),
        "parameters never reach a span"
      );
    },
  )
  .await;
}

/// The child half of [`the_sampler_comes_from_the_environment`].
#[test]
fn sampler_from_this_environment() {
  if std::env::var_os("LEDGER_SAMPLER_CHILD").is_none() {
    return;
  }
  assert_eq!(
    observability::sampler().unwrap(),
    OtelSampler::ParentBasedTraceIdRatio(0.25)
  );
}

#[test]
fn the_sampler_comes_from_the_environment() {
  // The default without one; the environment's in a process that has one (this binary again).
  if std::env::var_os("OTEL_TRACES_SAMPLER").is_none() {
    assert_eq!(
      observability::sampler().unwrap(),
      OtelSampler::ParentBasedAlwaysOn
    );
  }
  let child = std::process::Command::new(std::env::current_exe().unwrap())
    .args(["--exact", "sampler_from_this_environment", "--nocapture"])
    .env("LEDGER_SAMPLER_CHILD", "1")
    .env("OTEL_TRACES_SAMPLER", "parentbased_traceidratio")
    .env("OTEL_TRACES_SAMPLER_ARG", "0.25")
    .output()
    .unwrap();
  assert!(
    child.status.success(),
    "{}",
    String::from_utf8_lossy(&child.stdout)
  );
  assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
}

#[tokio::test]
async fn logs_never_show_passwords_tokens_or_secrets() {
  let config = test_config().await;
  let policy = observability::redaction(&config);
  let secret = config.auth.jwt_secret.expose().clone();
  let record = LogRecord::new(LogLevel::Warn, format!("signing with {secret}"))
    .with_annotation("password", "hunter2")
    .with_annotation("new_password", "hunter3")
    .with_annotation("refresh_token", "abc")
    .with_annotation("Authorization", "Bearer eyJ")
    .with_annotation("user", "carol");
  let out = policy.apply(&record);
  assert_eq!(out.message, "signing with <redacted>");
  for key in ["password", "new_password", "refresh_token", "Authorization"] {
    assert_eq!(out.annotations[key], "<redacted>", "{key}");
  }
  assert_eq!(out.annotations["user"], "carol");
}
