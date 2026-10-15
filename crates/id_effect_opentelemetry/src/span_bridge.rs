//! Bridges `next` spans ([`Effect::with_span`]) to OpenTelemetry spans.
//!
//! [`OtelSpanHook`] is a [`SpanHook`]: install it around the root effect with
//! [`Effect::with_span_hook`]. The hook lives in a `FiberRef` (inherited by every forked
//! child), so every `with_span` in the program, on whichever executor thread it runs, opens an
//! OpenTelemetry span; nothing goes through thread-locals or the `tracing` subscriber.
//!
//! - The OpenTelemetry span's parent is the enclosing `next` span's OpenTelemetry context, or,
//!   for a root span, the hook's parent context ([`OtelSpanHook::with_parent`], e.g. one
//!   extracted from W3C headers).
//! - Its name, attributes and start / end times are the `next` span's (executor clock).
//! - Its status follows the exit: success is `Ok`, a failure or a defect is `Error`, an
//!   interrupt leaves the status unset and sets `id_effect.interrupted = true`.
//! - The OpenTelemetry [`Context`] is kept in the span's extension slot:
//!   [`span_otel_context`] / [`current_otel_context`] read it back (to inject W3C headers into
//!   an outgoing request, for example).
//!
//! ```
//! use id_effect::Exit;
//! use id_effect::next::{Effect, succeed};
//! use id_effect_opentelemetry::{OtelSpanHook, sdk_tracer_provider_with_in_memory_exporter};
//! use opentelemetry_sdk::trace::InMemorySpanExporter;
//!
//! let exporter = InMemorySpanExporter::default();
//! let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
//! let hook = OtelSpanHook::for_provider(&provider, "docs").into_hook();
//! let eff: Effect<u8, ()> = succeed(1).with_span("work").with_span_hook(hook);
//! assert!(matches!(eff.block_on(), Exit::Success(1)));
//! let spans = exporter.get_finished_spans().unwrap();
//! assert_eq!(spans[0].name, "work");
//! ```

use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use id_effect::next::{Effect, ExitKind, Span, SpanHook};
use opentelemetry::trace::{Status, TraceContextExt, Tracer, TracerProvider as _};
use opentelemetry::{Context, KeyValue};
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};

/// The OpenTelemetry context of a bridged span (the value of its extension slot).
#[derive(Clone)]
struct OtelSpanContext(Context);

/// The OpenTelemetry context of `span` (its OpenTelemetry span is active in it), when `span` was
/// opened under an [`OtelSpanHook`].
pub fn span_otel_context(span: &Span) -> Option<Context> {
  span.extension::<OtelSpanContext>().map(|c| c.0.clone())
}

/// The OpenTelemetry context of the running fiber's current span (an empty [`Context`] outside
/// any bridged span).
pub fn current_otel_context<E, R>() -> Effect<Context, E, R>
where
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Span::current().map(|span| {
    span
      .as_ref()
      .and_then(span_otel_context)
      .unwrap_or_default()
  })
}

/// Executor-clock instant to wall-clock time (OpenTelemetry timestamps).
fn system_time(at: Instant) -> SystemTime {
  let (now, wall) = (Instant::now(), SystemTime::now());
  match now.checked_duration_since(at) {
    Some(ago) => wall.checked_sub(ago).unwrap_or(wall),
    None => wall + at.duration_since(now),
  }
}

/// A [`SpanHook`] exporting every `next` span through an OpenTelemetry [`Tracer`]. Install it
/// around the root effect with [`Effect::with_span_hook`]: each `with_span` then opens an OTEL
/// span (parent: the enclosing span's OTEL context, else [`with_parent`](Self::with_parent)),
/// ended with status `Ok` on success, `Error` on a failure or defect, and the attribute
/// `id_effect.interrupted = true` on interrupt.
#[derive(Clone)]
pub struct OtelSpanHook<T = SdkTracer> {
  tracer: T,
  parent: Context,
}

impl<T> std::fmt::Debug for OtelSpanHook<T> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("OtelSpanHook").finish_non_exhaustive()
  }
}

impl OtelSpanHook<SdkTracer> {
  /// A hook on `provider`'s tracer for instrumentation scope `scope`.
  pub fn for_provider(provider: &SdkTracerProvider, scope: impl Into<Cow<'static, str>>) -> Self {
    Self::new(provider.tracer(scope))
  }
}

impl<T> OtelSpanHook<T>
where
  T: Tracer + Send + Sync + 'static,
  T::Span: Send + Sync + 'static,
{
  /// A hook on `tracer`; root spans start a new trace.
  pub fn new(tracer: T) -> Self {
    Self {
      tracer,
      parent: Context::new(),
    }
  }

  /// Root spans (no enclosing bridged span) get `cx` as their parent, e.g. a context extracted
  /// from incoming W3C headers ([`extract_trace_context_from_headers`](crate::extract_trace_context_from_headers)).
  pub fn with_parent(mut self, cx: Context) -> Self {
    self.parent = cx;
    self
  }

  /// This hook as the value [`Effect::with_span_hook`] takes.
  pub fn into_hook(self) -> Arc<dyn SpanHook> {
    Arc::new(self)
  }
}

impl<T> SpanHook for OtelSpanHook<T>
where
  T: Tracer + Send + Sync + 'static,
  T::Span: Send + Sync + 'static,
{
  fn on_start(&self, span: &Span) {
    let parent = span
      .parent()
      .and_then(span_otel_context)
      .unwrap_or_else(|| self.parent.clone());
    let attributes: Vec<KeyValue> = span
      .attributes()
      .iter()
      .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
      .collect();
    let otel = self
      .tracer
      .span_builder(span.name().to_owned())
      .with_start_time(system_time(span.start()))
      .with_attributes(attributes)
      .start_with_context(&self.tracer, &parent);
    let _ = span.set_extension(OtelSpanContext(parent.with_span(otel)));
  }

  fn on_end(&self, span: &Span, end: Instant, exit: ExitKind) {
    let Some(OtelSpanContext(cx)) = span.extension::<OtelSpanContext>() else {
      return;
    };
    let otel = cx.span();
    match exit {
      ExitKind::Success => otel.set_status(Status::Ok),
      ExitKind::Failure => otel.set_status(Status::error("failure")),
      ExitKind::Die => otel.set_status(Status::error("defect")),
      ExitKind::Interrupt => otel.set_attribute(KeyValue::new("id_effect.interrupted", true)),
    }
    otel.end_with_timestamp(system_time(end));
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::propagation::{
    extract_trace_context_from_headers, inject_trace_context_into_headers, install_w3c_propagators,
  };
  use crate::subscriber::sdk_tracer_provider_with_in_memory_exporter;
  use id_effect::Exit;
  use id_effect::next::{Concurrency, die, fail, fork_all, succeed};
  use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};

  fn setup() -> (InMemorySpanExporter, SdkTracerProvider, Arc<dyn SpanHook>) {
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let hook = OtelSpanHook::for_provider(&provider, "test").into_hook();
    (exporter, provider, hook)
  }

  fn finished(exporter: &InMemorySpanExporter, name: &str) -> SpanData {
    exporter
      .get_finished_spans()
      .expect("spans")
      .into_iter()
      .find(|s| s.name == name)
      .unwrap_or_else(|| panic!("span {name} not exported"))
  }

  #[test]
  fn nested_spans_link_under_their_parent() {
    let (exporter, provider, hook) = setup();
    let eff: Effect<u8, ()> = succeed(7)
      .with_span_attrs("inner", [("k", "v")])
      .with_span("outer")
      .with_span_hook(hook);
    assert!(matches!(eff.block_on(), Exit::Success(7)));
    let outer = finished(&exporter, "outer");
    let inner = finished(&exporter, "inner");
    assert_eq!(inner.parent_span_id, outer.span_context.span_id());
    assert_eq!(inner.span_context.trace_id(), outer.span_context.trace_id());
    assert!(
      inner
        .attributes
        .iter()
        .any(|kv| kv.key.as_str() == "k" && kv.value.as_str() == "v")
    );
    assert_eq!(outer.status, Status::Ok);
    assert!(inner.start_time <= inner.end_time);
    let _ = provider.shutdown();
  }

  #[test]
  fn failures_and_defects_set_error_status() {
    let (exporter, provider, hook) = setup();
    let failing: Effect<(), &str> = fail("boom")
      .with_span("failing")
      .with_span_hook(hook.clone());
    assert!(matches!(failing.block_on(), Exit::Failure(_)));
    let dying: Effect<(), ()> = die("bug").with_span("dying").with_span_hook(hook);
    assert!(matches!(dying.block_on(), Exit::Failure(_)));
    assert!(matches!(
      finished(&exporter, "failing").status,
      Status::Error { .. }
    ));
    assert!(matches!(
      finished(&exporter, "dying").status,
      Status::Error { .. }
    ));
    let _ = provider.shutdown();
  }

  #[test]
  fn forked_children_inherit_the_hook_and_parent_on_worker_threads() {
    let (exporter, provider, hook) = setup();
    let rt = id_effect_tokio::TokioRuntime::new_multi_thread().expect("tokio");
    let children: Vec<Effect<u8, ()>> = (0..4u8)
      .map(|i| succeed(i).with_span(format!("child_{i}")))
      .collect();
    let eff: Effect<Vec<u8>, ()> = fork_all(children, Concurrency::Unbounded)
      .with_span("parent")
      .with_span_hook(hook);
    assert!(matches!(rt.block_on(rt.run(eff)), Exit::Success(v) if v.len() == 4));
    let parent = finished(&exporter, "parent");
    for i in 0..4 {
      let child = finished(&exporter, &format!("child_{i}"));
      assert_eq!(child.parent_span_id, parent.span_context.span_id());
      assert_eq!(
        child.span_context.trace_id(),
        parent.span_context.trace_id()
      );
    }
    let _ = provider.shutdown();
  }

  #[test]
  fn root_spans_continue_a_remote_parent_and_expose_their_context() {
    install_w3c_propagators();
    let (exporter, provider, _) = setup();
    let remote = {
      let tracer = provider.tracer("remote");
      Context::new().with_span(tracer.start("remote"))
    };
    let mut headers = Vec::new();
    inject_trace_context_into_headers(&remote, &mut headers);
    let parent = extract_trace_context_from_headers(&Context::new(), &headers);
    let hook = OtelSpanHook::for_provider(&provider, "test")
      .with_parent(parent)
      .into_hook();
    let eff: Effect<Vec<(String, String)>, ()> = current_otel_context()
      .map(|cx| {
        let mut out = Vec::new();
        inject_trace_context_into_headers(&cx, &mut out);
        out
      })
      .with_span("handler")
      .with_span_hook(hook);
    let Exit::Success(outgoing) = eff.block_on() else {
      panic!("handler failed")
    };
    let handler = finished(&exporter, "handler");
    assert_eq!(
      handler.span_context.trace_id(),
      remote.span().span_context().trace_id()
    );
    assert_eq!(
      handler.parent_span_id,
      remote.span().span_context().span_id()
    );
    let traceparent = outgoing
      .iter()
      .find(|(k, _)| k == "traceparent")
      .map(|(_, v)| v.clone())
      .expect("traceparent injected");
    assert!(traceparent.contains(&handler.span_context.span_id().to_string()));
    let _ = provider.shutdown();
  }

  #[test]
  fn no_hook_exports_nothing() {
    let (exporter, provider, _) = setup();
    let eff: Effect<(), ()> = succeed(()).with_span("unhooked");
    assert!(matches!(eff.block_on(), Exit::Success(())));
    assert!(exporter.get_finished_spans().expect("spans").is_empty());
    let _ = provider.shutdown();
  }
}
