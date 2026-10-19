//! The small public surface around the transports: error messages, `Debug` output that keeps
//! secrets out, configuration accessors, the `TestMailbox` layer and template failures. Nothing
//! here leaves the process: sends go to the in-memory `TestMailbox`.

use std::path::PathBuf;
use std::time::Duration;

use id_effect::{Cause, Effect, Exit};
use id_effect_mail::{
  Address, AddressError, InlinePart, MailError, MailerEnv, MailerService, Message, SendGridConfig,
  SmtpConfig, SmtpMailer, SmtpSecurity, TemplateError, Templates, TestMailbox, deliver,
};
use id_effect_tokio::TokioRuntime;

async fn run<A: Send + 'static>(eff: Effect<A, MailError>) -> Result<A, MailError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn message() -> Message {
  Message::builder()
    .from(Address::parse("billing@ledger.example").unwrap())
    .to(Address::parse("carol@ledger.example").unwrap())
    .subject("Hello")
    .text("Hi")
    .build()
    .unwrap()
}

/// A fresh directory under the system temp dir, removed by the caller.
fn scratch_dir(tag: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_nanos();
  let dir = std::env::temp_dir().join(format!(
    "id_effect_mail_{tag}_{}_{nanos}",
    std::process::id()
  ));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

#[test]
fn address_errors_explain_the_problem() {
  let long_local = format!("{}@x.example", "l".repeat(65));
  let long_address = format!("a@{}.example", "d".repeat(250));
  let cases = [
    ("a b@x.example", AddressError::Whitespace, "whitespace"),
    ("nobody", AddressError::MissingAt, "no `@`"),
    ("@x.example", AddressError::EmptyLocal, "nothing before"),
    (
      long_local.as_str(),
      AddressError::LocalTooLong,
      "longer than 64 bytes",
    ),
    ("a@x..example", AddressError::InvalidDomain, "dot-separated"),
    (
      long_address.as_str(),
      AddressError::TooLong,
      "longer than 254 bytes",
    ),
  ];
  for (input, error, words) in cases {
    assert_eq!(Address::parse(input), Err(error), "{input}");
    assert!(error.to_string().contains(words), "{error}");
  }
}

#[test]
fn mail_errors_name_their_kind() {
  let cases = [
    (
      MailError::InvalidMessage("no body".into()),
      "invalid mail message: no body",
    ),
    (
      MailError::Transport("reset".into()),
      "mail transport failed: reset",
    ),
    (
      MailError::Unauthorized("535".into()),
      "mail credentials refused: 535",
    ),
    (
      MailError::Temporary {
        code: 451,
        message: "later".into(),
      },
      "mail temporarily refused (451): later",
    ),
  ];
  for (error, text) in cases {
    assert_eq!(error.to_string(), text);
  }
}

#[tokio::test]
async fn the_test_mailbox_layer_records_what_is_delivered() {
  let mailbox = TestMailbox::new();
  let sent = run(deliver::<MailerEnv, _>(message()).provide_layer(mailbox.layer()))
    .await
    .expect("accepted");
  assert!(sent.id.is_some());
  assert_eq!(mailbox.len(), 1);
  let shown = format!("{mailbox:?}");
  assert_eq!(shown, "TestMailbox { sent: 1, queued_failures: 0 }");
  mailbox.fail_next(MailError::Transport("down".into()));
  assert!(format!("{mailbox:?}").contains("queued_failures: 1"));
  let service = MailerService::new(mailbox.clone());
  assert_eq!(format!("{service:?}"), "MailerService(\"<dyn Mailer>\")");
}

#[test]
fn inline_parts_debug_without_their_bytes() {
  let part = InlinePart::new("logo", "image/png", vec![0u8; 3]).with_filename("logo.png");
  assert_eq!(
    format!("{part:?}"),
    "InlinePart { content_id: \"logo\", content_type: \"image/png\", filename: Some(\"logo.png\"), len: 3 }"
  );
}

#[test]
fn smtp_settings_are_kept_and_shown_without_the_password() {
  let config = SmtpConfig::plaintext("relay.local", 2525)
    .credentials("mailer", "s3cret-pass")
    .timeout(Duration::from_secs(5))
    .pool_min_idle(2)
    .pool_idle_timeout(Duration::from_secs(9))
    .hello_name("mx.ledger.example");
  assert_eq!(config.host(), "relay.local");
  assert_eq!(config.security(), SmtpSecurity::Plaintext);
  assert_eq!(config.username(), Some("mailer"));
  assert_eq!(SmtpSecurity::Plaintext.default_port(), 25);
  let shown = format!("{config:?}");
  for part in [
    "timeout: 5s",
    "pool_min_idle: 2",
    "pool_idle_timeout: 9s",
    "hello_name: Some(\"mx.ledger.example\")",
  ] {
    assert!(shown.contains(part), "{part}: {shown}");
  }
  assert!(!shown.contains("s3cret-pass"));
  let mailer = SmtpMailer::new(config).expect("valid config");
  assert_eq!(mailer.config().effective_port(), 2525);
}

#[test]
fn sendgrid_settings_are_kept() {
  let config = SendGridConfig::new("SG.key")
    .base_url("https://eu.api.sendgrid.example/")
    .sandbox(true)
    .timeout(Duration::from_secs(3));
  assert!(config.is_sandbox());
  assert_eq!(
    config.send_url(),
    "https://eu.api.sendgrid.example/v3/mail/send"
  );
  let shown = format!("{config:?}");
  assert!(shown.contains("timeout: Some(3s)"), "{shown}");
  assert!(!shown.contains("SG.key"), "{shown}");
}

#[test]
fn template_registries_report_their_failures() {
  let empty = Templates::default();
  assert_eq!(empty.dir(), None);
  assert!(!empty.reloads());
  assert!(format!("{empty:?}").starts_with("Templates { dir: None, reload: false"));

  let missing = std::env::temp_dir().join("id_effect_mail_no_such_template_dir");
  let Err(io) = Templates::from_dir(&missing) else {
    panic!("a missing directory is refused");
  };
  assert!(matches!(io, TemplateError::Io(_)));
  assert!(
    io.to_string()
      .starts_with("mail templates: template directory "),
    "{io}"
  );

  let dir = scratch_dir("broken");
  std::fs::write(dir.join("broken.html"), "{% if %}").unwrap();
  let templates = Templates::from_dir(&dir).unwrap().reload(false);
  let err = templates.render("broken", &()).unwrap_err();
  let TemplateError::Render { template, .. } = &err else {
    panic!("a syntax error in a file is a render error: {err}");
  };
  assert_eq!(template, "broken.html");
  assert!(
    err.to_string().starts_with("mail template `broken.html`: "),
    "{err}"
  );
  assert!(matches!(MailError::from(err), MailError::InvalidMessage(_)));
  std::fs::remove_dir_all(dir).unwrap();
}
