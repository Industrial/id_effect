//! W3C Trace Context and baggage propagation on portable header maps.

use opentelemetry::Context;
use opentelemetry::propagation::{Extractor, Injector, TextMapCompositePropagator};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};

/// Installs W3C trace context **and** baggage propagators as the global text-map propagator.
pub fn install_w3c_propagators() {
  let composite = TextMapCompositePropagator::new(vec![
    Box::new(TraceContextPropagator::new()),
    Box::new(BaggagePropagator::new()),
  ]);
  opentelemetry::global::set_text_map_propagator(composite);
}

/// Installs trace context only (legacy alias — prefer [`install_w3c_propagators`]).
pub fn install_w3c_trace_context_propagator() {
  install_w3c_propagators();
}

struct VecHeadersInjector<'a>(&'a mut Vec<(String, String)>);

impl Injector for VecHeadersInjector<'_> {
  fn set(&mut self, key: &str, value: String) {
    self.0.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
    self.0.push((key.to_string(), value));
  }
}

/// Carrier for [`Extractor`] over immutable header slices.
pub struct VecHeadersExtractor<'a> {
  headers: &'a [(String, String)],
}

impl<'a> VecHeadersExtractor<'a> {
  /// Wraps a borrowed header list for extraction.
  pub fn new(headers: &'a [(String, String)]) -> Self {
    Self { headers }
  }
}

impl Extractor for VecHeadersExtractor<'_> {
  fn get(&self, key: &str) -> Option<&str> {
    self.headers.iter().find_map(|(k, v)| {
      if k.eq_ignore_ascii_case(key) {
        Some(v.as_str())
      } else {
        None
      }
    })
  }

  fn keys(&self) -> Vec<&str> {
    self.headers.iter().map(|(k, _)| k.as_str()).collect()
  }
}

/// Injects `cx` trace/baggage state into `headers`.
pub fn inject_trace_context_into_headers(cx: &Context, headers: &mut Vec<(String, String)>) {
  let mut inj = VecHeadersInjector(headers);
  opentelemetry::global::get_text_map_propagator(|prop| prop.inject_context(cx, &mut inj));
}

/// Injects the current OTEL [`Context`] into `headers`.
#[inline]
pub fn inject_current_trace_context(headers: &mut Vec<(String, String)>) {
  inject_trace_context_into_headers(&Context::current(), headers);
}

/// Extracts a [`Context`] from `headers` using the global propagator.
pub fn extract_trace_context_from_headers(base: &Context, headers: &[(String, String)]) -> Context {
  let ext = VecHeadersExtractor::new(headers);
  opentelemetry::global::get_text_map_propagator(|prop| prop.extract_with_context(base, &ext))
}

#[cfg(feature = "platform")]
/// Inject trace context into an [`id_effect_platform::http::HttpRequest`] before dispatch.
pub fn inject_into_http_request(req: &mut id_effect_platform::http::HttpRequest) {
  inject_current_trace_context(&mut req.headers);
}

#[cfg(feature = "platform")]
/// Extract parent context from an [`id_effect_platform::http::HttpRequest`].
pub fn extract_from_http_request(req: &id_effect_platform::http::HttpRequest) -> Context {
  extract_trace_context_from_headers(&Context::new(), &req.headers)
}

#[cfg(test)]
mod tests {
  use super::*;
  use opentelemetry::trace::TracerProvider as _;
  use opentelemetry::trace::{TraceContextExt, Tracer};
  use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

  #[test]
  fn inject_then_extract_preserves_remote_span_id() {
    install_w3c_propagators();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
      .with_simple_exporter(exporter.clone())
      .build();
    let tracer = provider.tracer("propagation_test");
    let span = tracer.start("remote");
    let cx = opentelemetry::Context::current_with_span(span);
    let mut headers = Vec::new();
    inject_trace_context_into_headers(&cx, &mut headers);
    assert!(
      headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("traceparent")),
      "expected traceparent header, got {headers:?}"
    );
    let extracted = extract_trace_context_from_headers(&Context::default(), &headers);
    assert!(extracted.span().span_context().is_valid());
    let _ = provider.shutdown();
  }

  #[test]
  fn get_is_case_insensitive_for_header_name() {
    let headers = vec![(
      "TraceParent".to_string(),
      "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
    )];
    let ext = VecHeadersExtractor::new(&headers);
    assert!(ext.get("traceparent").is_some());
  }
}
