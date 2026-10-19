//! End-to-end checks of the logger pipeline: redaction before fan-out, sampling backends.

use std::io;
use std::sync::{Arc, Mutex};

use id_effect::schema::Redacted;
use id_effect::{Effect, Exit, HNil, succeed};
use id_effect_logger::{
  CompositeLogBackend, EffectLogger, EffectLoggerError, JsonLogBackend, LogBackend, LogLevel,
  LogMetadataLive, LogRecord, Logger, RedactionPolicy, SamplingBackend, StructuredLogBackend,
  TracingLogBackend, annotate_logs, provide_composite_logger, provide_redaction_policy,
  with_log_span,
};

/// The composite backend and the redaction policy are process-wide: the tests that install them
/// run one at a time when `cargo test` runs them on threads of one process.
static PIPELINE: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
  PIPELINE
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Stands in for a network backend (e.g. the OTel bridge): keeps every record it is handed.
#[derive(Default)]
struct Capture(Mutex<Vec<LogRecord<'static>>>);

impl LogBackend for Capture {
  fn emit(&self, rec: &LogRecord<'_>) -> Result<(), EffectLoggerError> {
    let mut owned = LogRecord::new(rec.level, rec.message.to_string())
      .with_annotations(rec.annotations.clone())
      .with_spans(rec.spans.clone());
    owned.target = rec.target.as_deref().map(|t| t.to_string().into());
    self.0.lock().unwrap().push(owned);
    Ok(())
  }
}

impl Capture {
  fn rendered(&self) -> String {
    format!("{:?}", self.0.lock().unwrap())
  }
}

/// `io::Write` into a shared buffer, for the `tracing` subscriber.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl io::Write for SharedBuf {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.0.lock().unwrap().extend_from_slice(buf);
    Ok(buf.len())
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl SharedBuf {
  fn text(&self) -> String {
    String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
  }
}

#[test]
fn redacted_field_never_reaches_any_backend() {
  let _serial = serial();
  let api_key = Redacted::new("sk-live-42".to_string());
  let policy = RedactionPolicy::dangerous_empty()
    .field("password")
    .glob("*token*")
    .redacted(&api_key);

  let json = JsonLogBackend::new(Vec::<u8>::new());
  let structured = StructuredLogBackend::new(Vec::<u8>::new());
  let capture = Arc::new(Capture::default());
  let composite = Arc::new(CompositeLogBackend::new());
  composite.add(Arc::new(json.clone())).unwrap();
  composite.add(Arc::new(structured.clone())).unwrap();
  composite.add(Arc::new(TracingLogBackend)).unwrap();
  composite.add(capture.clone()).unwrap();

  let pipeline = LogMetadataLive::layer()
    .merge(provide_composite_logger(composite))
    .merge(provide_redaction_policy(policy));
  let program: Effect<(), EffectLoggerError, HNil> = annotate_logs(
    "Password",
    "hunter2",
    annotate_logs(
      "refresh_token",
      "tok-123",
      annotate_logs(
        "user",
        "alice",
        with_log_span(
          "login",
          EffectLogger.log_target(
            "auth::login",
            LogLevel::Info,
            format!("accepted key {}", api_key.expose()),
          ),
        ),
      ),
    ),
  );

  let traced = SharedBuf::default();
  let writer = traced.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_ansi(false)
    .with_max_level(tracing::Level::TRACE)
    .with_writer(move || writer.clone())
    .finish();
  let exit =
    tracing::subscriber::with_default(subscriber, || program.provide_layer(pipeline).block_on());
  assert_eq!(exit, Exit::Success(()));

  let outputs = [
    (
      "json",
      String::from_utf8(json.writer_arc().lock().unwrap().clone()).unwrap(),
    ),
    (
      "structured",
      String::from_utf8(structured.writer_arc().lock().unwrap().clone()).unwrap(),
    ),
    ("tracing", traced.text()),
    ("capture", capture.rendered()),
  ];
  for (name, out) in &outputs {
    for secret in ["hunter2", "tok-123", "sk-live-42"] {
      assert!(!out.contains(secret), "{name} saw {secret:?}: {out}");
    }
    assert!(out.contains("<redacted>"), "{name} has no mask: {out}");
    assert!(out.contains("alice"), "{name} lost a clean field: {out}");
    assert!(
      out.contains("accepted key"),
      "{name} lost the message: {out}"
    );
  }
  let recs = capture.0.lock().unwrap();
  assert_eq!(recs.len(), 1);
  assert_eq!(recs[0].target.as_deref(), Some("auth::login"));
  assert_eq!(recs[0].spans, vec!["login".to_string()]);
}

#[test]
fn rate_limit_drops_and_counts() {
  let _serial = serial();
  let capture = Arc::new(Capture::default());
  let sampled = Arc::new(
    SamplingBackend::new(capture.clone())
      .rate_limit(3, 0.0)
      .exempt_at_or_above(LogLevel::Error),
  );
  let composite = Arc::new(CompositeLogBackend::new());
  composite.add(sampled.clone()).unwrap();

  let mut program: Effect<(), EffectLoggerError, HNil> = succeed(());
  for i in 0..10 {
    program = program.then(EffectLogger.info(format!("burst {i}")));
  }
  program = program.then(EffectLogger.error("still delivered"));
  let exit = program
    .provide_layer(provide_composite_logger(composite))
    .block_on();
  assert_eq!(exit, Exit::Success(()));

  let stats = sampled.stats();
  assert_eq!(stats.emitted, 4);
  assert_eq!(stats.rate_limited, 7);
  assert_eq!(stats.sampled_out, 0);
  assert_eq!(stats.dropped(), 7);
  let msgs: Vec<String> = capture
    .0
    .lock()
    .unwrap()
    .iter()
    .map(|r| r.message.to_string())
    .collect();
  assert_eq!(msgs, ["burst 0", "burst 1", "burst 2", "still delivered"]);
}
