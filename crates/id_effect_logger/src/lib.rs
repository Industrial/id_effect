//! Injectable [`EffectLogger`] service for the effect system.
//!
//! # Service/Tag pattern
//!
//! Extract the logger from the environment once with `~EffectLogger`, then call
//! its methods as regular effectful steps — no turbofish:
//!
//! ```
//! use id_effect::Exit;
//! use id_effect::{Effect, caps, effect};
//! use id_effect_logger::{EffectLogger, EffectLoggerError};
//!
//! fn program() -> Effect<u32, EffectLoggerError, caps!(EffectLogger)> {
//!   effect!({
//!     let logger = ~EffectLogger;
//!     ~logger.warn("something suspicious");
//!     ~logger.info("all good");
//!     7
//!   })
//! }
//!
//! assert_eq!(program().provide(EffectLogger).block_on(), Exit::Success(7));
//! ```
//!
//! Since 0.7 every effect in this crate is an [`id_effect::Effect`] (the 0.7 kernel). Log
//! methods return `Effect<(), EffectLoggerError>` (no environment, `R = HNil`): `~` binds them in
//! any `effect!` environment. In combinator chains, lift them with
//! [`Effect::narrow`](id_effect::Effect::narrow).
//!
//! The environment only needs the `EffectLogger` capability (`caps!(EffectLogger)`). Provide
//! it at the program edge with `.provide(EffectLogger)`.
//!
//! The thread-local pipeline (minimum level, annotations, spans, composite backend) is installed
//! by building its [`Layer`](id_effect::Layer)s ([`provide_minimum_log_level`],
//! [`LogMetadataLive::layer`], [`provide_composite_logger`]) on the thread that runs the program;
//! the installed refs are [`FiberRef`]s:
//!
//! ```
//! use std::sync::Arc;
//!
//! use id_effect::Exit;
//! use id_effect::{Effect, HNil};
//! use id_effect_logger::{
//!   CompositeLogBackend, EffectLogger, EffectLoggerError, LogLevel, LogMetadataLive,
//!   annotate_logs, provide_composite_logger, provide_minimum_log_level,
//! };
//!
//! let pipeline = LogMetadataLive::layer()
//!   .merge(provide_composite_logger(Arc::new(CompositeLogBackend::new())))
//!   .merge(provide_minimum_log_level(LogLevel::Info));
//! let program: Effect<(), EffectLoggerError, HNil> =
//!   annotate_logs("request_id", "r-1", EffectLogger.info("handled"));
//! // The layer is built (and the pipeline installed) before the program runs.
//! let exit = program.provide_layer(pipeline).block_on();
//! assert_eq!(exit, Exit::Success(()));
//! ```
//!
//! Log methods accept `impl Into<Cow<'static, str>>`: literals stay zero-copy;
//! runtime text passes as `String` or `format!(...)`.

#![deny(missing_docs)]

use core::convert::Infallible;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::LocalKey;

use ::id_effect::{Effect, FiberRef, fail, succeed, suspend};

mod pipeline;
mod providers;

pub use pipeline::{
  CompositeLogBackend, JsonLogBackend, LogBackend, LogRecord, Logger, StructuredLogBackend,
  TracingLogBackend,
};

pub use providers::{
  EffectLogComposite, EffectLogMetadata, EffectLoggerLive, LogMetadataLive,
  provide_composite_logger, provide_minimum_log_level,
};

use pipeline::TracingLogBackend as TracingSink;

thread_local! {
  static MIN_LOG_LEVEL_FIBER_REF: RefCell<Option<FiberRef<LogLevel>>> = const { RefCell::new(None) };
}

thread_local! {
  static COMPOSITE_LOG_BACKEND: RefCell<Option<Arc<CompositeLogBackend>>> = const { RefCell::new(None) };
}

thread_local! {
  static LOG_ANNOTATIONS_FIBER_REF: RefCell<Option<FiberRef<HashMap<String, String>>>> =
    const { RefCell::new(None) };
}

thread_local! {
  static LOG_SPAN_STACK_FIBER_REF: RefCell<Option<FiberRef<Vec<String>>>> = const { RefCell::new(None) };
}

fn install_min_log_level_fiber_ref(fr: FiberRef<LogLevel>) {
  MIN_LOG_LEVEL_FIBER_REF.with(|c| {
    *c.borrow_mut() = Some(fr);
  });
}

fn install_composite_log_backend(c: Arc<CompositeLogBackend>) {
  COMPOSITE_LOG_BACKEND.with(|cell| {
    *cell.borrow_mut() = Some(c);
  });
}

fn install_log_annotations_fiber_ref(fr: FiberRef<HashMap<String, String>>) {
  LOG_ANNOTATIONS_FIBER_REF.with(|c| {
    *c.borrow_mut() = Some(fr);
  });
}

fn install_log_spans_fiber_ref(fr: FiberRef<Vec<String>>) {
  LOG_SPAN_STACK_FIBER_REF.with(|c| {
    *c.borrow_mut() = Some(fr);
  });
}

/// The installed ref on this thread (cloned handle), if any.
fn installed<T: Clone + Send + Sync + 'static>(
  slot: &'static LocalKey<RefCell<Option<FiberRef<T>>>>,
) -> Option<FiberRef<T>> {
  slot.with(|c| c.borrow().clone())
}

/// The current fiber's value of the ref installed in `slot`, or `None` when nothing is installed
/// on the running thread.
fn current<T: Clone + Send + Sync + 'static>(
  slot: &'static LocalKey<RefCell<Option<FiberRef<T>>>>,
) -> Effect<Option<T>, EffectLoggerError> {
  match installed(slot) {
    None => succeed(None),
    Some(fr) => fr.get().map(Some),
  }
}

/// Hands `rec` to the installed composite backend, or to `tracing` when none is installed.
fn emit_record(rec: &LogRecord<'_>) -> Result<(), EffectLoggerError> {
  COMPOSITE_LOG_BACKEND.with(|c| match c.borrow().as_ref() {
    Some(comp) => comp.emit_all(rec),
    None => LogBackend::emit(&TracingSink, rec),
  })
}

#[cfg(test)]
fn test_clear_min_log_level_fiber_ref() {
  MIN_LOG_LEVEL_FIBER_REF.with(|c| {
    *c.borrow_mut() = None;
  });
}

#[cfg(test)]
fn test_clear_composite_log_backend() {
  COMPOSITE_LOG_BACKEND.with(|c| {
    *c.borrow_mut() = None;
  });
}

#[cfg(test)]
fn test_clear_log_metadata_fiber_refs() {
  LOG_ANNOTATIONS_FIBER_REF.with(|c| *c.borrow_mut() = None);
  LOG_SPAN_STACK_FIBER_REF.with(|c| *c.borrow_mut() = None);
}

#[cfg(test)]
fn test_clear_all_logger_tls() {
  test_clear_min_log_level_fiber_ref();
  test_clear_composite_log_backend();
  test_clear_log_metadata_fiber_refs();
}

/// Log sink provided as the [`EffectLogger`] capability (`.provide(EffectLogger)` or
/// [`EffectLoggerLive::layer`]); forwards to [`tracing`].
///
/// Extracted from the environment with `~EffectLogger` inside
/// [`effect!`](id_effect::effect). After extraction its methods return
/// `Effect<(), EffectLoggerError>` (no environment) and are themselves bound with `~` — in any
/// `effect!` environment, without a turbofish.
#[derive(Clone, Copy, Debug, Default)]
pub struct EffectLogger;

/// Fiber-local minimum log level installed by [`provide_minimum_log_level`].
pub type EffectLogMinLevel = FiberRef<LogLevel>;

/// Errors that a log sink may produce.
///
/// Currently the only backend is [`tracing`], which is infallible, so no
/// variant is ever constructed at runtime.  The type exists so that callers
/// can compose it into their `E` bound and gain compile-time proof that the
/// logger's error channel is handled, without changing the API when a
/// fallible backend (e.g. a network sink) is added later.
#[derive(Debug, Clone, ::id_effect::EffectData)]
pub enum EffectLoggerError {
  /// The underlying log sink returned an error.
  Sink(String),
}

impl std::fmt::Display for EffectLoggerError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      EffectLoggerError::Sink(msg) => write!(f, "log sink error: {msg}"),
    }
  }
}

impl std::error::Error for EffectLoggerError {}

impl From<Infallible> for EffectLoggerError {
  fn from(e: Infallible) -> Self {
    match e {}
  }
}

/// Metadata attached to log lines (wall-clock UTC, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogContext {
  /// Wall-clock instant in UTC when the log context was created or captured.
  pub timestamp: ::id_effect::UtcDateTime,
}

impl LogContext {
  /// Build a context with an explicit UTC timestamp.
  #[inline]
  pub const fn new(timestamp: ::id_effect::UtcDateTime) -> Self {
    Self { timestamp }
  }

  /// Capture the current system time as UTC.
  #[inline]
  pub fn with_now_timestamp() -> Self {
    Self {
      timestamp: ::id_effect::UtcDateTime::from_std(std::time::SystemTime::now())
        .expect("system time should be representable as UtcDateTime"),
    }
  }
}

/// Logging level for [`EffectLogger`] (defined in core since 0.6; re-exported here).
pub use id_effect::observability::LogLevel;

impl EffectLogger {
  /// Run `inner` with this fiber’s minimum log level overridden to `level` ([`FiberRef::locally`]).
  pub fn with_minimum_log_level<B, E, R>(
    fiber_ref: FiberRef<LogLevel>,
    level: LogLevel,
    inner: Effect<B, E, R>,
  ) -> Effect<B, E, R>
  where
    B: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    fiber_ref.locally(level, inner)
  }

  /// Emit a log line at `level`.  Returns an effect that, when run, forwards
  /// to [`tracing`].  The logger is self-contained after extraction, so the effect needs no
  /// environment (`R = HNil`):
  ///
  /// - inside [`id_effect::effect!`], `~logger.log(…)` binds in any environment — no
  ///   turbofish;
  /// - in combinator chains over an environment `R`, lift it with
  ///   [`Effect::narrow`](id_effect::Effect::narrow).
  ///
  /// When [`provide_minimum_log_level`] has been built on this thread, messages below the current
  /// fiber’s minimum [`LogLevel`] (from that [`FiberRef`]) are dropped without calling [`tracing`].
  ///
  /// `msg` may be a `&'static str`, `String`, or other `Into<Cow<'static, str>>`.
  pub fn log(
    &self,
    level: LogLevel,
    msg: impl Into<Cow<'static, str>>,
  ) -> Effect<(), EffectLoggerError> {
    let msg = msg.into();
    if level == LogLevel::None {
      return succeed(());
    }
    // The thread-locals are read when the effect runs (on the thread driving the fiber), and the
    // installed refs are read from the running fiber.
    suspend(move || {
      current(&MIN_LOG_LEVEL_FIBER_REF).flat_map(move |min| {
        let emit = match min {
          None => true,
          Some(min) => min.allows(level),
        };
        if !emit {
          return succeed(());
        }
        current(&LOG_ANNOTATIONS_FIBER_REF)
          .zip(current(&LOG_SPAN_STACK_FIBER_REF))
          .flat_map(move |(annotations, spans)| {
            let rec = LogRecord {
              level,
              message: msg,
              annotations: annotations.unwrap_or_default(),
              spans: spans.unwrap_or_default(),
            };
            match emit_record(&rec) {
              Ok(()) => succeed(()),
              Err(e) => fail(e),
            }
          })
      })
    })
  }

  /// Same as [`Self::log`]; kept for call sites that already hold a [`String`].
  #[inline]
  pub fn log_string(&self, level: LogLevel, msg: String) -> Effect<(), EffectLoggerError> {
    self.log(level, msg)
  }

  /// Shorthand for [`Self::log`] at [`LogLevel::Trace`].
  pub fn trace(&self, msg: impl Into<Cow<'static, str>>) -> Effect<(), EffectLoggerError> {
    self.log(LogLevel::Trace, msg)
  }

  /// Shorthand for [`Self::log`] at [`LogLevel::Debug`].
  pub fn debug(&self, msg: impl Into<Cow<'static, str>>) -> Effect<(), EffectLoggerError> {
    self.log(LogLevel::Debug, msg)
  }

  /// Shorthand for [`Self::log`] at [`LogLevel::Info`].
  pub fn info(&self, msg: impl Into<Cow<'static, str>>) -> Effect<(), EffectLoggerError> {
    self.log(LogLevel::Info, msg)
  }

  /// Shorthand for [`Self::log`] at [`LogLevel::Warn`].
  pub fn warn(&self, msg: impl Into<Cow<'static, str>>) -> Effect<(), EffectLoggerError> {
    self.log(LogLevel::Warn, msg)
  }

  /// Shorthand for [`Self::log`] at [`LogLevel::Error`].
  pub fn error(&self, msg: impl Into<Cow<'static, str>>) -> Effect<(), EffectLoggerError> {
    self.log(LogLevel::Error, msg)
  }

  /// Shorthand for [`Self::log`] at [`LogLevel::Fatal`].
  pub fn fatal(&self, msg: impl Into<Cow<'static, str>>) -> Effect<(), EffectLoggerError> {
    self.log(LogLevel::Fatal, msg)
  }
}

/// Run `inner` with `key=value` merged into the fiber-local annotation map (restored afterward).
pub fn annotate_logs<A, E, R>(
  key: impl Into<String> + Send + 'static,
  value: impl Into<String> + Send + 'static,
  inner: Effect<A, E, R>,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let key = key.into();
  let value = value.into();
  suspend(move || match installed(&LOG_ANNOTATIONS_FIBER_REF) {
    None => inner,
    Some(fr) => fr.get().flat_map(move |mut next: HashMap<String, String>| {
      next.insert(key, value);
      fr.locally(next, inner)
    }),
  })
}

/// Run `inner` while a span label is pushed on the fiber-local span stack (restored afterward).
pub fn with_log_span<A, E, R>(
  label: impl Into<String> + Send + 'static,
  inner: Effect<A, E, R>,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let label = label.into();
  suspend(move || match installed(&LOG_SPAN_STACK_FIBER_REF) {
    None => inner,
    Some(fr) => fr.get().flat_map(move |mut next: Vec<String>| {
      next.push(label);
      fr.locally(next, inner)
    }),
  })
}

#[cfg(test)]
mod tests {
  use rstest::rstest;

  use super::*;
  use crate::{EffectLoggerLive, provide_minimum_log_level};
  use ::id_effect::{Cause, Never};
  use ::id_effect::{HCons, HList, HNil, Layer, need};

  // ========== Fixtures ==========

  /// Runs an effect on the calling thread (where this thread's logger refs are installed).
  pub(super) fn run<A: Send + 'static, E: Send + 'static>(
    eff: Effect<A, E>,
  ) -> Result<A, Cause<E>> {
    eff.block_on().into_result()
  }

  /// Builds `layer` on the calling thread (installing its thread-local logger state here).
  pub(super) fn build_layer<Out: HList>(layer: Layer<Out, Never>) -> Out {
    layer.build().block_on().into_result().expect("layer build")
  }

  type LogCtx = HCons<EffectLogger, HNil>;

  fn test_ctx() -> LogCtx {
    build_layer(EffectLoggerLive::layer())
  }

  /// Installs a fresh minimum-level ref starting at `initial` and returns it.
  fn test_ctx_with_min(initial: LogLevel) -> EffectLogMinLevel {
    test_clear_all_logger_tls();
    let HCons(fr, HNil) = build_layer(provide_minimum_log_level(initial));
    (*fr).clone()
  }

  fn init_subscriber() {
    test_clear_all_logger_tls();
    let _ = tracing_subscriber::fmt()
      .with_env_filter(tracing_subscriber::EnvFilter::new("trace"))
      .with_test_writer()
      .try_init();
  }

  // ========== fiber_min_log_level ==========

  mod fiber_min_log_level {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::Subscriber;
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::layer::{Context, Layer};

    struct Capture(Arc<Mutex<Vec<tracing::Level>>>);

    impl<S: Subscriber> Layer<S> for Capture {
      fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        self
          .0
          .lock()
          .expect("capture mutex")
          .push(*event.metadata().level());
      }
    }

    fn subscriber_with_capture(levels: Arc<Mutex<Vec<tracing::Level>>>) -> impl Subscriber {
      Registry::default().with(Capture(levels))
    }

    #[test]
    fn logger_filters_below_minimum_level() {
      test_clear_all_logger_tls();
      let levels = Arc::new(Mutex::new(Vec::new()));
      let _g = tracing::subscriber::set_default(subscriber_with_capture(levels.clone()));

      let fr = test_ctx_with_min(LogLevel::Trace);
      // A FiberRef value lives in the running fiber: set and log in one program.
      run(
        fr.set(LogLevel::Warn)
          .then(EffectLogger.info("filtered-info")),
      )
      .expect("log");

      let got = levels.lock().expect("capture");
      assert!(
        !got.contains(&tracing::Level::INFO),
        "expected INFO suppressed, got {got:?}"
      );
    }

    #[test]
    fn logger_with_minimum_log_level_overrides_globally() {
      test_clear_all_logger_tls();
      let levels = Arc::new(Mutex::new(Vec::new()));
      let _g = tracing::subscriber::set_default(subscriber_with_capture(levels.clone()));

      let fr = test_ctx_with_min(LogLevel::Trace);
      let inner = EffectLogger.info("inside-scope");
      run(EffectLogger::with_minimum_log_level(
        fr.clone(),
        LogLevel::Warn,
        inner,
      ))
      .expect("scoped");

      assert!(
        !levels
          .lock()
          .expect("capture")
          .contains(&tracing::Level::INFO),
        "expected INFO suppressed inside locally"
      );

      levels.lock().expect("capture").clear();
      let _ctx = test_ctx_with_min(LogLevel::Trace);
      run(EffectLogger.info("outside-scope")).expect("outer");

      assert!(
        levels
          .lock()
          .expect("capture")
          .contains(&tracing::Level::INFO),
        "expected INFO after new stack without Warn override"
      );
    }

    #[test]
    fn logger_restores_level_after_locally_scope() {
      test_clear_all_logger_tls();
      let _g =
        tracing::subscriber::set_default(subscriber_with_capture(Arc::new(Mutex::new(Vec::new()))));

      let fr = test_ctx_with_min(LogLevel::Trace);
      let scoped = EffectLogger::with_minimum_log_level(fr.clone(), LogLevel::Warn, fr.get());
      let program = fr
        .set(LogLevel::Debug)
        .then(fr.get())
        .zip(scoped)
        .zip(fr.get());
      assert_eq!(
        run::<_, Never>(program),
        Ok(((LogLevel::Debug, LogLevel::Warn), LogLevel::Debug))
      );
      // A new run starts from the ref's initial value (`provide_minimum_log_level(Trace)`).
      assert_eq!(run::<_, Never>(fr.get()), Ok(LogLevel::Trace));
    }

    #[test]
    fn forked_child_inherits_minimum_level() {
      test_clear_all_logger_tls();
      let levels = Arc::new(Mutex::new(Vec::new()));
      let _g = tracing::subscriber::set_default(subscriber_with_capture(levels.clone()));

      let fr = test_ctx_with_min(LogLevel::Trace);
      // The child reads the parent's `Error` level (copied at fork time).
      let program = fr.set(LogLevel::Error).then(
        fr.get::<EffectLoggerError, HNil>()
          .fork()
          .widen()
          .flat_map(|h| h.join()),
      );
      assert_eq!(run(program), Ok(LogLevel::Error));
    }
  }

  // ========== log_context ==========

  mod log_context {
    use super::*;
    use ::id_effect::UtcDateTime;

    #[test]
    fn log_context_timestamp_format_iso() {
      let ctx = LogContext::new(UtcDateTime::unsafe_make(1_700_000_000_000));
      let s = ctx.timestamp.format_iso();
      assert!(
        s.ends_with('Z'),
        "format_iso should be UTC / RFC 3339 style: {s}"
      );
      assert!(s.contains('T'), "expected date-time separator: {s}");
    }

    #[test]
    fn log_context_with_now_timestamp_is_valid_iso() {
      let ctx = LogContext::with_now_timestamp();
      let s = ctx.timestamp.format_iso();
      assert!(s.ends_with('Z'), "{s}");
      assert!(s.contains('T'), "{s}");
    }
  }

  // ========== effect_logger_log ==========

  mod effect_logger_log {
    use super::*;

    mod with_unit_env {
      use super::*;

      #[rstest]
      #[case::trace(LogLevel::Trace)]
      #[case::debug(LogLevel::Debug)]
      #[case::info(LogLevel::Info)]
      #[case::warn(LogLevel::Warn)]
      #[case::error(LogLevel::Error)]
      #[case::fatal(LogLevel::Fatal)]
      fn returns_ok_for_every_level(#[case] level: LogLevel) {
        init_subscriber();
        let result = run(EffectLogger.log(level, "msg"));
        assert_eq!(result, Ok(()));
      }
    }

    mod with_context_env {
      use super::*;

      #[rstest]
      #[case::trace(LogLevel::Trace)]
      #[case::debug(LogLevel::Debug)]
      #[case::info(LogLevel::Info)]
      #[case::warn(LogLevel::Warn)]
      #[case::error(LogLevel::Error)]
      #[case::fatal(LogLevel::Fatal)]
      fn returns_ok_for_every_level(#[case] level: LogLevel) {
        init_subscriber();
        // Extract the logger from a typed environment, then bind the log step.
        let program: Effect<(), EffectLoggerError, HCons<EffectLogger, HNil>> =
          need::<EffectLogger>().flat_map(move |logger| logger.log(level, "msg").narrow());
        let result = run(program.provide(EffectLogger));
        assert_eq!(result, Ok(()));
      }
    }
  }

  // ========== effect_logger_level_methods ==========

  mod no_tls_paths {
    use super::*;

    #[test]
    fn annotate_logs_without_tls_still_runs_inner() {
      crate::test_clear_all_logger_tls();
      let result = run(annotate_logs("k", "v", succeed::<i32, (), HNil>(42)));
      assert_eq!(result, Ok(42));
    }

    #[test]
    fn with_log_span_without_tls_still_runs_inner() {
      crate::test_clear_all_logger_tls();
      let result = run(with_log_span("span", succeed::<i32, (), HNil>(99)));
      assert_eq!(result, Ok(99));
    }
  }

  mod effect_logger_level_methods {
    use super::*;

    #[test]
    fn trace_delegates_to_log_at_trace_level() {
      init_subscriber();
      assert_eq!(run(EffectLogger.trace("t")), Ok(()));
    }

    #[test]
    fn debug_delegates_to_log_at_debug_level() {
      init_subscriber();
      assert_eq!(run(EffectLogger.debug("d")), Ok(()));
    }

    #[test]
    fn info_delegates_to_log_at_info_level() {
      init_subscriber();
      assert_eq!(run(EffectLogger.info("i")), Ok(()));
    }

    #[test]
    fn warn_delegates_to_log_at_warn_level() {
      init_subscriber();
      assert_eq!(run(EffectLogger.warn("w")), Ok(()));
    }

    #[test]
    fn error_delegates_to_log_at_error_level() {
      init_subscriber();
      assert_eq!(run(EffectLogger.error("e")), Ok(()));
    }

    #[test]
    fn log_string_delegates_at_info_level() {
      init_subscriber();
      assert_eq!(
        run(EffectLogger.log_string(LogLevel::Info, "owned msg".to_string()),),
        Ok(()),
      );
    }

    #[test]
    fn info_accepts_formatted_string() {
      init_subscriber();
      let n = 42u32;
      assert_eq!(run(EffectLogger.info(format!("n={n}"))), Ok(()),);
    }

    #[test]
    fn fatal_delegates_to_log_at_fatal_level() {
      init_subscriber();
      assert_eq!(run(EffectLogger.fatal("f")), Ok(()));
    }

    #[test]
    fn log_none_level_returns_ok_without_side_effects() {
      init_subscriber();
      assert_eq!(run(EffectLogger.log(LogLevel::None, "silenced")), Ok(()));
    }
  }

  // ========== into_bind_extraction ==========

  mod needs_extraction {
    use super::*;

    #[test]
    fn extracts_logger_copy_from_context() {
      // The layer-built typed env.
      let HCons(_logger, HNil): LogCtx = test_ctx();
      let effect: Effect<EffectLogger, EffectLoggerError, HCons<EffectLogger, HNil>> =
        need::<EffectLogger>().map(|l| *l);
      assert!(run(effect.provide(EffectLogger)).is_ok());
    }

    #[test]
    fn extracted_logger_can_emit_log() {
      init_subscriber();
      let effect: Effect<EffectLogger, EffectLoggerError, HCons<EffectLogger, HNil>> =
        need::<EffectLogger>().map(|l| *l);
      let logger = run(effect.provide(EffectLogger)).expect("extraction is infallible");
      assert_eq!(run(logger.info("extracted")), Ok(()));
    }
  }

  // ========== EffectLoggerLive provider ==========

  mod effect_logger_live_provider {
    use super::*;

    #[test]
    fn builds_without_error() {
      let env = EffectLoggerLive::layer::<Never>().build().block_on();
      assert!(env.into_result().is_ok());
    }

    #[test]
    fn produced_service_can_be_placed_in_context() {
      let HCons(logger, HNil) = test_ctx();
      let result = run(logger.info("provider build ok"));
      assert_eq!(result, Ok(()));
    }

    #[test]
    fn layer_provides_the_logger_to_a_program() {
      let program: Effect<(), EffectLoggerError, LogCtx> =
        need::<EffectLogger>().flat_map(|logger| logger.info("via layer").narrow());
      assert_eq!(
        run(program.provide_layer(EffectLoggerLive::layer())),
        Ok(())
      );
    }
  }

  // ========== effect_logger_error ==========

  mod effect_logger_error {
    use super::*;

    #[test]
    fn sink_variant_display_contains_message() {
      let err = EffectLoggerError::Sink("oops".to_owned());
      assert!(err.to_string().contains("oops"));
    }

    #[test]
    fn sink_variant_display_has_prefix() {
      let err = EffectLoggerError::Sink("x".to_owned());
      assert!(err.to_string().starts_with("log sink error:"));
    }

    #[test]
    fn sink_variant_implements_error_trait() {
      let err: Box<dyn std::error::Error> = Box::new(EffectLoggerError::Sink("e".to_owned()));
      assert!(err.to_string().contains("e"));
    }

    #[test]
    fn two_equal_sink_errors_are_eq() {
      assert_eq!(
        EffectLoggerError::Sink("a".to_owned()),
        EffectLoggerError::Sink("a".to_owned())
      );
    }

    #[test]
    fn two_different_sink_errors_are_ne() {
      assert_ne!(
        EffectLoggerError::Sink("a".to_owned()),
        EffectLoggerError::Sink("b".to_owned())
      );
    }
  }

  // ========== beads 263w: Logger pipeline + backends ==========

  mod wave5_full_logger {
    use std::sync::{Arc, Mutex};

    use super::{build_layer, run};
    use crate::{
      CompositeLogBackend, EffectLogger, EffectLoggerError, JsonLogBackend, LogBackend, LogLevel,
      LogMetadataLive, LogRecord, Logger, StructuredLogBackend, annotate_logs,
      provide_composite_logger, with_log_span,
    };

    struct MsgCap(Arc<Mutex<Vec<String>>>);

    impl LogBackend for MsgCap {
      fn emit(&self, rec: &LogRecord<'_>) -> Result<(), EffectLoggerError> {
        self.0.lock().expect("cap").push(rec.message.to_string());
        Ok(())
      }
    }

    fn setup_json() -> Arc<Mutex<Vec<u8>>> {
      crate::test_clear_all_logger_tls();
      let jb = JsonLogBackend::new(Vec::<u8>::new());
      let buf = jb.writer_arc();
      let comp = Arc::new(CompositeLogBackend::new());
      comp.add(Arc::new(jb)).expect("add json");
      build_layer(LogMetadataLive::layer().merge(provide_composite_logger(comp)));
      buf
    }

    #[test]
    fn logger_json_backend_produces_valid_json() {
      let buf = setup_json();
      run(annotate_logs("k", "v", EffectLogger.info("hello"))).expect("log");
      let bytes = buf.lock().expect("buf");
      let line = std::str::from_utf8(bytes.as_slice()).expect("utf8");
      let line = line.trim();
      let v: serde_json::Value = serde_json::from_str(line).expect("valid json");
      assert_eq!(v["level"], "INFO");
      assert_eq!(v["message"], "hello");
      assert_eq!(v["fields"]["k"], "v");
    }

    #[test]
    fn logger_add_replaces_layer() {
      crate::test_clear_all_logger_tls();
      let a = Arc::new(Mutex::new(Vec::new()));
      let b = Arc::new(Mutex::new(Vec::new()));
      let comp = Arc::new(CompositeLogBackend::new());
      comp.add(Arc::new(MsgCap(a.clone()))).unwrap();
      comp.add(Arc::new(MsgCap(b.clone()))).unwrap();
      build_layer(LogMetadataLive::layer().merge(provide_composite_logger(comp.clone())));
      run(EffectLogger.info("m1")).unwrap();
      assert_eq!(*a.lock().unwrap(), vec!["m1".to_string()]);
      assert_eq!(*b.lock().unwrap(), vec!["m1".to_string()]);

      let c = Arc::new(Mutex::new(Vec::new()));
      comp.replace(0, Arc::new(MsgCap(c.clone()))).unwrap();
      a.lock().unwrap().clear();
      b.lock().unwrap().clear();
      run(EffectLogger.info("m2")).unwrap();
      assert!(a.lock().unwrap().is_empty());
      assert_eq!(*b.lock().unwrap(), vec!["m2".to_string()]);
      assert_eq!(*c.lock().unwrap(), vec!["m2".to_string()]);
    }

    #[test]
    fn logger_fatal_is_highest_level() {
      assert!(LogLevel::Fatal.severity() > LogLevel::Error.severity());
      assert!(LogLevel::Trace.allows(LogLevel::Fatal));
      assert!(!LogLevel::Fatal.allows(LogLevel::Error));
      assert!(LogLevel::Fatal.allows(LogLevel::Fatal));
      assert!(!LogLevel::None.allows(LogLevel::Info));
    }

    #[test]
    fn annotate_logs_visible_in_structured_output() {
      crate::test_clear_all_logger_tls();
      let structured = StructuredLogBackend::new(Vec::<u8>::new());
      let buf = structured.writer_arc();
      let comp = Arc::new(CompositeLogBackend::new());
      comp.add(Arc::new(structured)).unwrap();
      build_layer(LogMetadataLive::layer().merge(provide_composite_logger(comp)));
      run(annotate_logs("trace_id", "abc", EffectLogger.info("done"))).unwrap();
      let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
      assert!(
        s.contains("trace_id") && s.contains("abc"),
        "expected annotation in output: {s:?}"
      );
    }

    #[test]
    fn with_log_span_visible_in_json() {
      let buf = setup_json();
      run(with_log_span("outer", EffectLogger.warn("inside"))).unwrap();
      let bytes = buf.lock().unwrap().clone();
      let line = std::str::from_utf8(&bytes).unwrap().trim();
      let v: serde_json::Value = serde_json::from_str(line).unwrap();
      assert_eq!(v["spans"], serde_json::json!(["outer"]));
    }
  }
}
