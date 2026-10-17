//! The process-wide minimum level: set from any thread, it overrides the fiber-local minimum
//! until cleared. Its own test binary, since the level is process state.

use std::sync::{Arc, Mutex};

use id_effect::{Effect, Exit, HNil};
use id_effect_logger::{
  CompositeLogBackend, EffectLogger, EffectLoggerError, LogBackend, LogLevel, LogRecord, Logger,
  global_minimum_log_level, provide_composite_logger, provide_minimum_log_level,
  set_global_minimum_log_level,
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

impl Capture {
  fn take(&self) -> Vec<String> {
    std::mem::take(&mut *self.0.lock().unwrap())
  }
}

/// Logs one line per level from debug to error through a pipeline whose fiber-local minimum is
/// `Info`.
fn log_each_level(capture: &Arc<Capture>) -> Vec<String> {
  let composite = Arc::new(CompositeLogBackend::new());
  composite.add(capture.clone()).unwrap();
  let pipeline =
    provide_composite_logger(composite).merge(provide_minimum_log_level(LogLevel::Info));
  let program: Effect<(), EffectLoggerError, HNil> = EffectLogger
    .debug("debug")
    .then(EffectLogger.info("info"))
    .then(EffectLogger.warn("warn"))
    .then(EffectLogger.error("error"));
  assert!(matches!(
    program.provide_layer(pipeline).block_on(),
    Exit::Success(())
  ));
  capture.take()
}

#[test]
fn global_minimum_overrides_the_fiber_local_minimum_until_cleared() {
  let capture = Arc::new(Capture::default());
  assert_eq!(global_minimum_log_level(), None);
  assert_eq!(log_each_level(&capture), ["info", "warn", "error"]);

  // Set from another thread: the change is process-wide.
  std::thread::spawn(|| set_global_minimum_log_level(Some(LogLevel::Debug)))
    .join()
    .unwrap();
  assert_eq!(global_minimum_log_level(), Some(LogLevel::Debug));
  assert_eq!(log_each_level(&capture), ["debug", "info", "warn", "error"]);

  set_global_minimum_log_level(Some(LogLevel::Warn));
  assert_eq!(log_each_level(&capture), ["warn", "error"]);

  set_global_minimum_log_level(Some(LogLevel::None));
  assert_eq!(global_minimum_log_level(), Some(LogLevel::None));
  assert!(log_each_level(&capture).is_empty());

  set_global_minimum_log_level(None);
  assert_eq!(global_minimum_log_level(), None);
  assert_eq!(log_each_level(&capture), ["info", "warn", "error"]);
}
