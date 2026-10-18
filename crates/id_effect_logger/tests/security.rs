//! Regression tests for the logger findings of the 0.10 security review: redaction is on by
//! default (M11) and a logged value cannot forge a line or a field (M12). Its own test binary:
//! the M11 pipeline test installs the process-wide composite.

use std::fmt;
use std::sync::{Arc, Mutex};

use id_effect::observability::posture::dangerous_opt_outs;
use id_effect::{Effect, Exit, HNil};
use id_effect_logger::{
  CompositeLogBackend, DEFAULT_REDACTION_MASK, EffectLogger, EffectLoggerError, JsonLogBackend,
  LogBackend, LogLevel, LogMetadataLive, LogRecord, Logger, RedactionPolicy, StructuredLogBackend,
  TracingLogBackend, active_redaction_policy, annotate_logs, provide_composite_logger,
};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// Keeps every record it is handed, rendered with `Debug`.
#[derive(Default)]
struct Capture(Mutex<Vec<String>>);

impl LogBackend for Capture {
  fn emit(&self, rec: &LogRecord<'_>) -> Result<(), EffectLoggerError> {
    self.0.lock().unwrap().push(format!("{rec:?}"));
    Ok(())
  }
}

/// Keeps the raw `message` of every `tracing` event: the line [`TracingLogBackend`] formatted,
/// before any subscriber formatting could escape it.
#[derive(Clone, Default)]
struct RawLines(Arc<Mutex<Vec<String>>>);

impl<S: Subscriber> Layer<S> for RawLines {
  fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
    struct Message(String);
    impl Visit for Message {
      fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
          self.0 = format!("{value:?}");
        }
      }
    }
    let mut message = Message(String::new());
    event.record(&mut message);
    self.0.lock().unwrap().push(message.0);
  }
}

/// The line [`TracingLogBackend`] hands to `tracing` for `rec`.
fn tracing_line(rec: &LogRecord<'_>) -> String {
  let lines = RawLines::default();
  let subscriber = Registry::default().with(lines.clone());
  tracing::subscriber::with_default(subscriber, || TracingLogBackend.emit(rec)).unwrap();
  let mut got = lines.0.lock().unwrap().clone();
  assert_eq!(got.len(), 1, "one event per record: {got:?}");
  got.remove(0)
}

/// What [`StructuredLogBackend`] writes for `rec`.
fn structured_line(rec: &LogRecord<'_>) -> String {
  let backend = StructuredLogBackend::new(Vec::<u8>::new());
  backend.emit(rec).unwrap();
  String::from_utf8(backend.writer_arc().lock().unwrap().clone()).unwrap()
}

#[test]
fn m11_default_pipeline_redacts_common_secret_fields() {
  let secrets = [
    ("Password", "pw-1"),
    ("passwd", "pw-2"),
    ("user_password_hash", "pw-3"),
    ("SECRET", "sec-1"),
    ("client_secret", "sec-2"),
    ("webhook_secret", "sec-3"),
    ("token", "tok-1"),
    ("access_token", "tok-2"),
    ("refresh_token", "tok-3"),
    ("id_token", "tok-4"),
    ("github_token", "tok-5"),
    ("api_key", "key-1"),
    ("ApiKey", "key-2"),
    ("Authorization", "Bearer abc"),
    ("cookie", "sid=1"),
    ("Set-Cookie", "sid=2"),
    ("session", "sess-1"),
    ("csrf", "csrf-1"),
    ("private_key", "pk-1"),
  ];

  let json = JsonLogBackend::new(Vec::<u8>::new());
  let structured = StructuredLogBackend::new(Vec::<u8>::new());
  let capture = Arc::new(Capture::default());
  let composite = Arc::new(CompositeLogBackend::new());
  composite.add(Arc::new(json.clone())).unwrap();
  composite.add(Arc::new(structured.clone())).unwrap();
  composite.add(capture.clone()).unwrap();
  // No `provide_redaction_policy`: the default applies.
  let pipeline = LogMetadataLive::layer().merge(provide_composite_logger(composite));

  let program: Effect<(), EffectLoggerError, HNil> = secrets.iter().fold(
    annotate_logs("user", "alice", EffectLogger.info("signed in")),
    |inner, (k, v)| annotate_logs(*k, *v, inner),
  );
  assert_eq!(
    program.provide_layer(pipeline).block_on(),
    Exit::Success(())
  );

  let outputs = [
    (
      "json",
      String::from_utf8(json.writer_arc().lock().unwrap().clone()).unwrap(),
    ),
    (
      "structured",
      String::from_utf8(structured.writer_arc().lock().unwrap().clone()).unwrap(),
    ),
    ("capture", capture.0.lock().unwrap().join("\n")),
  ];
  for (name, out) in &outputs {
    for (key, value) in secrets {
      assert!(!out.contains(value), "{name} saw {key}={value:?}: {out}");
    }
    assert_eq!(
      out.matches(DEFAULT_REDACTION_MASK).count(),
      secrets.len(),
      "{name}: {out}"
    );
    assert!(out.contains("alice"), "{name} lost a clean field: {out}");
  }

  let active = active_redaction_policy();
  assert!(!active.is_empty());
  for key in ["password", "CSRF_Token", "db_password", "Cookie"] {
    assert!(active.redacts_field(key), "{key}");
  }
  for key in ["user", "request_id", "tokens_used", "secretary"] {
    assert!(!active.redacts_field(key), "{key}");
  }
}

#[test]
#[allow(deprecated)]
fn m11_empty_policy_is_explicit() {
  let rec = LogRecord::new(LogLevel::Info, "m")
    .with_annotation("password", "hunter2")
    .with_annotation("access_token", "tok");

  // The empty policy redacts nothing, and says so in its name; the deprecated `new` is it.
  for policy in [RedactionPolicy::empty(), RedactionPolicy::new()] {
    assert!(policy.is_empty());
    let out = policy.apply(&rec);
    assert_eq!(out.annotations["password"], "hunter2");
    assert_eq!(out.annotations["access_token"], "tok");
  }

  // `Default` and the pipeline default are the recommended rules.
  for policy in [RedactionPolicy::default(), RedactionPolicy::recommended()] {
    assert!(!policy.is_empty());
    let out = policy.apply(&rec);
    assert_eq!(out.annotations["password"], DEFAULT_REDACTION_MASK);
    assert_eq!(out.annotations["access_token"], DEFAULT_REDACTION_MASK);
  }

  // Turning redaction off is a recorded opt-out.
  let id = "id_effect_logger::RedactionPolicy::dangerous_none";
  let none = RedactionPolicy::dangerous_none();
  assert!(none.is_empty());
  assert_eq!(none.apply(&rec).annotations["password"], "hunter2");
  assert!(
    dangerous_opt_outs().iter().any(|(got, _)| *got == id),
    "{:?}",
    dangerous_opt_outs()
  );
}

#[test]
fn m12_newline_in_message_cannot_forge_a_line() {
  let rec = LogRecord::new(
    LogLevel::Info,
    "login failed for bob\nINFO auth: login ok user=admin\r\n",
  )
  .with_target("auth\nINFO forged")
  .with_spans(vec!["req\n1".into()])
  .with_annotation("reason", "bad password\nERROR forged");

  let line = tracing_line(&rec);
  assert!(!line.contains(['\n', '\r']), "{line:?}");
  assert_eq!(
    line,
    r#"auth\nINFO forged: [req\n1] login failed for bob\nINFO auth: login ok user=admin\r\n reason="bad password\nERROR forged""#
  );

  let structured = structured_line(&rec);
  assert_eq!(structured.lines().count(), 1, "{structured:?}");
  assert!(structured.ends_with('\n') && !structured.contains('\r'));
}

#[test]
fn m12_annotation_key_cannot_inject_fields() {
  let rec = LogRecord::new(LogLevel::Info, "m")
    .with_annotation("user admin=true", "bob")
    .with_annotation("role", "guest is_admin=true");

  let line = tracing_line(&rec);
  assert!(
    !line.contains(" admin=true"),
    "key injected a field: {line}"
  );
  assert!(line.contains(r"user\u{20}admin\u{3d}true=bob"), "{line}");
  // A value with a space is quoted: `is_admin=true` stays inside `role`.
  assert!(line.contains(r#"role="guest is_admin=true""#), "{line}");

  let structured = structured_line(&rec);
  assert!(!structured.contains(" admin=true"), "{structured}");
  assert!(
    structured.contains(r#"user\u{20}admin\u{3d}true="bob""#),
    "{structured}"
  );
}

#[test]
fn m12_control_and_ansi_sequences_are_escaped() {
  let rec = LogRecord::new(
    LogLevel::Warn,
    "\u{1b}[31mred\u{1b}[0m\u{7}\u{8}\u{0}\u{7f}\u{9b}2J\u{2028}",
  )
  .with_annotation("title\u{1b}]0;x\u{7}", "\u{1b}]0;pwned\u{7}");

  let raw = [
    '\u{1b}', '\u{7}', '\u{8}', '\u{0}', '\u{7f}', '\u{9b}', '\u{2028}',
  ];
  let line = tracing_line(&rec);
  assert!(!line.contains(raw), "{line:?}");
  assert_eq!(
    line,
    r"\u{1b}[31mred\u{1b}[0m\u{7}\u{8}\u{0}\u{7f}\u{9b}2J\u{2028} title\u{1b}]0;x\u{7}=\u{1b}]0;pwned\u{7}"
  );

  let structured = structured_line(&rec);
  assert!(!structured.contains(raw), "{structured:?}");
  assert!(structured.contains(r"\u{1b}[31mred"), "{structured}");
}
