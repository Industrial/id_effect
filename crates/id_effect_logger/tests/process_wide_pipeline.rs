//! The redaction policy and the composite backend are process-wide: installed once on the
//! startup thread, they apply to lines logged from std threads and tokio worker threads too (a
//! server's request handlers). Its own test binary, since both are process state.

use std::sync::{Arc, Mutex};

use id_effect::schema::Redacted;
use id_effect::{Effect, Exit, HNil, Never, Runtime};
use id_effect_logger::{
  CompositeLogBackend, EffectLogger, EffectLoggerError, JsonLogBackend, LogBackend, LogRecord,
  Logger, RedactionPolicy, provide_composite_logger, provide_redaction_policy,
};

/// Keeps the message of every record it is handed.
#[derive(Default)]
struct Capture(Mutex<Vec<String>>);

impl LogBackend for Capture {
  fn emit(&self, rec: &LogRecord<'_>) -> Result<(), EffectLoggerError> {
    self.0.lock().unwrap().push(rec.message.to_string());
    Ok(())
  }
}

fn log_line(secret: &str, origin: &str) -> Effect<(), EffectLoggerError, HNil> {
  EffectLogger.info(format!("{origin} accepted key {secret}"))
}

fn log_secret(secret: &str, origin: &str) {
  assert_eq!(log_line(secret, origin).block_on(), Exit::Success(()));
}

#[test]
fn policy_and_composite_installed_on_one_thread_apply_on_every_thread() {
  let api_key = Redacted::new("sk-live-42".to_string());
  let secret = api_key.expose().clone();

  let json = JsonLogBackend::new(Vec::<u8>::new());
  let capture = Arc::new(Capture::default());
  let composite = Arc::new(CompositeLogBackend::new());
  composite.add(Arc::new(json.clone())).unwrap();
  composite.add(capture.clone()).unwrap();

  // Installed on this (the "startup") thread only, the way a service's main does it.
  let pipeline = provide_composite_logger::<Never>(composite).merge(provide_redaction_policy(
    RedactionPolicy::new().redacted(&api_key),
  ));
  pipeline.build().block_on().into_result().expect("pipeline");

  log_secret(&secret, "main");

  let threads: Vec<_> = (0..4)
    .map(|i| {
      let secret = secret.clone();
      std::thread::spawn(move || log_secret(&secret, &format!("std-{i}")))
    })
    .collect();
  for t in threads {
    t.join().unwrap();
  }

  let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(4)
    .build()
    .unwrap();
  rt.block_on(async {
    let tasks: Vec<_> = (0..8)
      .map(|i| {
        let secret = secret.clone();
        tokio::spawn(async move {
          assert_ne!(
            std::thread::current().name(),
            Some("main"),
            "must log from a worker"
          );
          // The async edge, the way a request handler runs its effect.
          let line = log_line(&secret, &format!("worker-{i}"));
          assert_eq!(Runtime::global().run(line).await, Exit::Success(()));
        })
      })
      .collect();
    for t in tasks {
      t.await.unwrap();
    }
  });

  let msgs = capture.0.lock().unwrap().clone();
  assert_eq!(
    msgs.len(),
    1 + 4 + 8,
    "every thread emits through the composite: {msgs:?}"
  );
  let json_out = String::from_utf8(json.writer_arc().lock().unwrap().clone()).unwrap();
  assert_eq!(json_out.lines().count(), 13, "{json_out}");
  for out in msgs.iter().map(String::as_str).chain([json_out.as_str()]) {
    assert!(!out.contains(&secret), "unredacted line: {out}");
    assert!(out.contains("<redacted>"), "no mask: {out}");
  }
}
