//! Traced outbound HTTP: [`TracedHttpClient`] opens a client span around every call of an
//! [`HttpClient`] and sends that span's W3C `traceparent` with the request.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use id_effect::{Effect, Span};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClient, HttpClientService, HttpMethod, HttpRequest, HttpResponse, RetryPolicy,
  RetryingHttpClient, StreamingHttpResponse,
};
use opentelemetry::trace::TraceContextExt;
use opentelemetry::{Context, KeyValue};

use crate::propagation::inject_into_http_request;
use crate::span_bridge::{mark_span_error, span_otel_context};

/// Query parameters whose values `url.full` replaces with `REDACTED` (signed-URL credentials).
const REDACTED_QUERY_KEYS: [&str; 4] = ["AWSAccessKeyId", "Signature", "sig", "X-Goog-Signature"];

/// One call of an [`HttpClient`] (`execute` or `execute_stream`).
type Call<T> = fn(&dyn HttpClient, HttpRequest) -> Effect<T, HttpError>;

/// An [`HttpClient`] that opens a client span per call and injects its W3C trace context.
///
/// The span is a core span ([`Effect::with_span_attrs`]), so it becomes an OpenTelemetry span of
/// kind `Client` under an [`OtelSpanHook`](crate::OtelSpanHook), a child of the caller's span.
/// It is named after the method (`GET`) and carries `http.request.method`, `url.full` (without
/// user info, signature query values `REDACTED`), `server.address`, `server.port` and, once
/// answered, `http.response.status_code`. A status `>= 400` or an [`HttpError`] ends it with
/// status `Error` and an `error.type`. The `traceparent` (and `baggage`) header is injected with
/// [`inject_into_http_request`] from the client span's OpenTelemetry context, so the server's
/// span becomes its child; without a hook the ambient [`Context::current`] is injected.
/// For [`execute_stream`](HttpClient::execute_stream) the span ends when the headers arrive.
///
/// # Retries
///
/// The traced client sits **outside** the retry: one call is one client span, however many
/// attempts it takes, and every attempt carries the same `traceparent`. That keeps the span tree
/// the shape of the program (one logical request per call site, its status the final outcome)
/// and gives the server one parent to join its attempts under. [`TracedHttpClient::with_retry`]
/// builds the order itself and records each attempt as an `http.client.attempt` **event** on the
/// client span (`http.request.resend_count`, then `http.response.status_code` or `error.type`),
/// so retries stay visible without a span per attempt. Wrapping an existing
/// [`RetryingHttpClient`] with [`TracedHttpClient::new`] also traces outside the retry, without
/// attempt events. (A retrying client around a traced one, the other order, gives a span per
/// attempt instead.)
///
/// ```
/// use id_effect::{Effect, Exit};
/// use id_effect_opentelemetry::{
///   OtelSpanHook, TracedHttpClient, install_w3c_propagators,
///   sdk_tracer_provider_with_in_memory_exporter,
/// };
/// use id_effect_platform::error::HttpError;
/// use id_effect_platform::http::{
///   HttpClient, HttpRequest, HttpResponse, RetryPolicy, StreamingHttpResponse,
/// };
/// use opentelemetry_sdk::trace::InMemorySpanExporter;
///
/// /// Answers 200 and echoes the `traceparent` it was sent.
/// struct Echo;
///
/// impl HttpClient for Echo {
///   fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
///     let body = req
///       .headers
///       .iter()
///       .find(|(k, _)| k == "traceparent")
///       .map(|(_, v)| v.clone().into_bytes())
///       .unwrap_or_default();
///     id_effect::succeed(HttpResponse { status: 200, headers: vec![], body })
///   }
///
///   fn execute_stream(&self, _: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
///     unimplemented!()
///   }
/// }
///
/// install_w3c_propagators();
/// let exporter = InMemorySpanExporter::default();
/// let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
/// let client = TracedHttpClient::with_retry(Echo, RetryPolicy::default());
/// let eff = client
///   .execute(HttpRequest::get("https://api.example.com/v1/items"))
///   .with_span("handler")
///   .with_span_hook(OtelSpanHook::for_provider(&provider, "docs").into_hook());
/// let Exit::Success(resp) = eff.block_on() else { panic!("call failed") };
/// let spans = exporter.get_finished_spans().unwrap();
/// let span = spans.iter().find(|s| s.name == "GET").unwrap();
/// let traceparent = String::from_utf8(resp.body).unwrap();
/// assert!(traceparent.contains(&span.span_context.span_id().to_string()));
/// ```
#[derive(Clone)]
pub struct TracedHttpClient {
  inner: Arc<dyn HttpClient>,
  retry: Option<RetryPolicy>,
}

impl std::fmt::Debug for TracedHttpClient {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TracedHttpClient")
      .field("retry", &self.retry)
      .finish_non_exhaustive()
  }
}

impl TracedHttpClient {
  /// Traces every call of `inner` (a [`RetryingHttpClient`] is traced outside its retry).
  #[inline]
  pub fn new(inner: impl HttpClient) -> Self {
    Self {
      inner: Arc::new(inner),
      retry: None,
    }
  }

  /// Traces every call of the installed capability (wrap it again with
  /// [`HttpClientService::new`]).
  #[inline]
  pub fn from_service(service: HttpClientService) -> Self {
    Self {
      inner: service.0,
      retry: None,
    }
  }

  /// Retries `inner`'s calls with `policy` inside the client span, recording each attempt as an
  /// `http.client.attempt` event on it.
  #[inline]
  pub fn with_retry(inner: impl HttpClient, policy: RetryPolicy) -> Self {
    Self {
      inner: Arc::new(inner),
      retry: Some(policy),
    }
  }

  fn run<T: Send + 'static>(
    &self,
    mut req: HttpRequest,
    call: Call<T>,
    status: fn(&T) -> u16,
  ) -> Effect<T, HttpError> {
    let client: Arc<dyn HttpClient> = match &self.retry {
      Some(policy) => Arc::new(RetryingHttpClient::new(
        Attempts {
          inner: Arc::clone(&self.inner),
          made: AtomicU32::new(0),
        },
        policy.clone(),
      )),
      None => Arc::clone(&self.inner),
    };
    let name = method_name(&req.method);
    let attributes = client_attributes(&req);
    Span::current()
      .flat_map(move |span| {
        inject(span.as_ref(), &mut req);
        let on_error = span.clone();
        call(&*client, req)
          .map(move |resp| {
            if let Some(span) = &span {
              record_status(span, status(&resp));
            }
            resp
          })
          .map_err(move |e| {
            if let Some(span) = &on_error {
              record_error(span, &e);
            }
            e
          })
      })
      .with_span_attrs(name, attributes)
  }
}

impl HttpClient for TracedHttpClient {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    self.run(req, |c, r| c.execute(r), |r| r.status)
  }

  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    self.run(req, |c, r| c.execute_stream(r), |r| r.status)
  }
}

/// The client under the retry of [`TracedHttpClient::with_retry`]: adds one event per attempt to
/// the client span (built per call, so `made` counts this call's attempts).
struct Attempts {
  inner: Arc<dyn HttpClient>,
  made: AtomicU32,
}

impl Attempts {
  fn attempt<T: Send + 'static>(
    &self,
    req: HttpRequest,
    call: Call<T>,
    status: fn(&T) -> u16,
  ) -> Effect<T, HttpError> {
    let resend = i64::from(self.made.fetch_add(1, Ordering::Relaxed));
    let eff = call(&*self.inner, req);
    Span::current()
      .map(|span| span.as_ref().and_then(span_otel_context))
      .flat_map(move |cx| {
        let on_error = cx.clone();
        eff
          .map(move |resp| {
            let code = i64::from(status(&resp));
            attempt_event(cx, resend, KeyValue::new("http.response.status_code", code));
            resp
          })
          .map_err(move |e| {
            let kind = KeyValue::new("error.type", error_type(&e));
            attempt_event(on_error, resend, kind);
            e
          })
      })
  }
}

fn attempt_event(cx: Option<Context>, resend: i64, outcome: KeyValue) {
  if let Some(cx) = cx {
    cx.span().add_event(
      "http.client.attempt",
      vec![KeyValue::new("http.request.resend_count", resend), outcome],
    );
  }
}

impl HttpClient for Attempts {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    self.attempt(req, |c, r| c.execute(r), |r| r.status)
  }

  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    self.attempt(req, |c, r| c.execute_stream(r), |r| r.status)
  }
}

/// Injects the client span's trace context into `req` (the ambient context without a hook).
fn inject(span: Option<&Span>, req: &mut HttpRequest) {
  match span.and_then(span_otel_context) {
    Some(cx) => {
      let _attached = cx.attach();
      inject_into_http_request(req);
    }
    None => inject_into_http_request(req),
  }
}

fn record_status(span: &Span, status: u16) {
  let Some(cx) = span_otel_context(span) else {
    return;
  };
  let otel = cx.span();
  otel.set_attribute(KeyValue::new(
    "http.response.status_code",
    i64::from(status),
  ));
  if status >= 400 {
    otel.set_attribute(KeyValue::new("error.type", status.to_string()));
    mark_span_error(span, format!("HTTP {status}"));
  }
}

fn record_error(span: &Span, e: &HttpError) {
  if let Some(cx) = span_otel_context(span) {
    cx.span()
      .set_attribute(KeyValue::new("error.type", error_type(e)));
  }
  mark_span_error(span, e.to_string());
}

fn error_type(e: &HttpError) -> &'static str {
  match e {
    HttpError::Reqwest(_) => "transport",
    HttpError::BodyTooLarge { .. } => "body_too_large",
    HttpError::InvalidRequest(_) => "invalid_request",
  }
}

fn method_name(method: &HttpMethod) -> &'static str {
  match method {
    HttpMethod::Get => "GET",
    HttpMethod::Post => "POST",
    HttpMethod::Put => "PUT",
    HttpMethod::Delete => "DELETE",
    HttpMethod::Patch => "PATCH",
  }
}

/// The client span's start attributes (`otel.kind` makes it a `Client` span).
fn client_attributes(req: &HttpRequest) -> Vec<(&'static str, String)> {
  let mut attributes = vec![
    ("otel.kind", "client".to_owned()),
    ("http.request.method", method_name(&req.method).to_owned()),
  ];
  let Ok(uri) = req.url.parse::<http::Uri>() else {
    return attributes;
  };
  if let Some(host) = uri.host() {
    attributes.push(("server.address", host.to_owned()));
  }
  let port = uri.port_u16().or(match uri.scheme_str() {
    Some("https") => Some(443),
    Some("http") => Some(80),
    _ => None,
  });
  if let Some(port) = port {
    attributes.push(("server.port", port.to_string()));
  }
  attributes.push(("url.full", redacted_url(&uri)));
  attributes
}

/// `uri` without user info, with the values of [`REDACTED_QUERY_KEYS`] replaced.
fn redacted_url(uri: &http::Uri) -> String {
  let mut out = String::new();
  if let Some(scheme) = uri.scheme_str() {
    out.push_str(scheme);
    out.push_str("://");
  }
  if let Some(authority) = uri.authority() {
    let host = authority.as_str().rsplit('@').next().unwrap_or_default();
    out.push_str(host);
  }
  out.push_str(uri.path());
  if let Some(query) = uri.query() {
    out.push('?');
    let pairs: Vec<String> = query
      .split('&')
      .map(|pair| match pair.split_once('=') {
        Some((key, _)) if REDACTED_QUERY_KEYS.contains(&key) => format!("{key}=REDACTED"),
        _ => pair.to_owned(),
      })
      .collect();
    out.push_str(&pairs.join("&"));
  }
  out
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use super::*;
  use crate::propagation::install_w3c_propagators;
  use crate::span_bridge::OtelSpanHook;
  use crate::subscriber::sdk_tracer_provider_with_in_memory_exporter;
  use id_effect::{Exit, SpanHook};
  use id_effect_platform::http::HttpBody;
  use opentelemetry::Value;
  use opentelemetry::trace::{SpanKind, Status, Tracer, TracerProvider as _};
  use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};

  /// Answers with the scripted statuses in turn (the last one repeats), recording each request's
  /// headers.
  struct Stub {
    statuses: Mutex<Vec<u16>>,
    seen: Seen,
  }

  /// The headers of every request a [`Stub`] answered.
  type Seen = Arc<Mutex<Vec<Vec<(String, String)>>>>;

  impl Stub {
    fn new(statuses: Vec<u16>) -> (Self, Seen) {
      let seen = Arc::new(Mutex::new(Vec::new()));
      let stub = Self {
        statuses: Mutex::new(statuses),
        seen: Arc::clone(&seen),
      };
      (stub, seen)
    }

    fn answer(&self, req: &HttpRequest) -> u16 {
      self.seen.lock().unwrap().push(req.headers.clone());
      let mut statuses = self.statuses.lock().unwrap();
      if statuses.len() > 1 {
        statuses.remove(0)
      } else {
        statuses[0]
      }
    }
  }

  impl HttpClient for Stub {
    fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
      let status = self.answer(&req);
      id_effect::succeed(HttpResponse {
        status,
        headers: Vec::new(),
        body: Vec::new(),
      })
    }

    fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
      let status = self.answer(&req);
      id_effect::succeed(StreamingHttpResponse {
        status,
        headers: Vec::new(),
        body: HttpBody::empty(),
      })
    }
  }

  struct Failing;

  impl HttpClient for Failing {
    fn execute(&self, _: HttpRequest) -> Effect<HttpResponse, HttpError> {
      id_effect::fail(HttpError::InvalidRequest("bad header".into()))
    }

    fn execute_stream(&self, _: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
      id_effect::fail(HttpError::InvalidRequest("bad header".into()))
    }
  }

  fn setup() -> (InMemorySpanExporter, SdkTracerProvider, Arc<dyn SpanHook>) {
    install_w3c_propagators();
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let hook = OtelSpanHook::for_provider(&provider, "client_test").into_hook();
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

  fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span
      .attributes
      .iter()
      .find(|kv| kv.key.as_str() == key)
      .map(|kv| kv.value.clone())
  }

  fn traceparent(headers: &[(String, String)]) -> String {
    headers
      .iter()
      .find(|(k, _)| k == "traceparent")
      .map(|(_, v)| v.clone())
      .expect("traceparent sent")
  }

  #[test]
  fn client_span_is_child_and_sends_traceparent() {
    let (exporter, provider, hook) = setup();
    let (stub, seen) = Stub::new(vec![200]);
    let client = TracedHttpClient::new(stub);
    let url = "https://ada:secret@api.example.com:8443/v1/items?sig=abc&page=2";
    let eff = client
      .execute(HttpRequest::get(url))
      .with_span("handler")
      .with_span_hook(hook);
    assert!(matches!(eff.block_on(), Exit::Success(r) if r.status == 200));

    let handler = finished(&exporter, "handler");
    let span = finished(&exporter, "GET");
    assert_eq!(span.parent_span_id, handler.span_context.span_id());
    assert_eq!(
      span.span_context.trace_id(),
      handler.span_context.trace_id()
    );
    assert_eq!(span.span_kind, SpanKind::Client);
    assert_eq!(span.status, Status::Ok);
    assert_eq!(
      attr(&span, "http.response.status_code"),
      Some(Value::I64(200))
    );
    assert_eq!(
      attr(&span, "url.full").map(|v| v.as_str().into_owned()),
      Some("https://api.example.com:8443/v1/items?sig=REDACTED&page=2".to_owned())
    );
    assert_eq!(
      attr(&span, "server.address"),
      Some(Value::from("api.example.com"))
    );
    assert_eq!(attr(&span, "server.port"), Some(Value::from("8443")));
    assert_eq!(attr(&span, "http.request.method"), Some(Value::from("GET")));
    assert!(
      attr(&span, "otel.kind").is_none(),
      "the kind is not exported"
    );

    // W3C: 00-<trace id>-<client span id>-<flags>.
    let sent = traceparent(&seen.lock().unwrap()[0]);
    let parts: Vec<&str> = sent.split('-').collect();
    assert_eq!(parts[1], span.span_context.trace_id().to_string());
    assert_eq!(parts[2], span.span_context.span_id().to_string());
    let _ = provider.shutdown();
  }

  #[test]
  fn error_status_and_transport_errors_mark_the_span() {
    let (exporter, provider, hook) = setup();
    let (stub, _) = Stub::new(vec![404]);
    let eff = TracedHttpClient::new(stub)
      .execute_stream(HttpRequest::post("http://svc/orders", b"{}".to_vec()))
      .with_span_hook(hook.clone());
    assert!(matches!(eff.block_on(), Exit::Success(r) if r.status == 404));
    let span = finished(&exporter, "POST");
    assert!(matches!(span.status, Status::Error { .. }));
    assert_eq!(attr(&span, "error.type"), Some(Value::from("404")));
    assert_eq!(attr(&span, "server.port"), Some(Value::from("80")));

    let eff = TracedHttpClient::new(Failing)
      .execute(HttpRequest::get("http://svc/"))
      .with_span_hook(hook);
    assert!(matches!(eff.block_on(), Exit::Failure(_)));
    let span = finished(&exporter, "GET");
    assert!(
      matches!(&span.status, Status::Error { description } if description.contains("bad header"))
    );
    assert_eq!(
      attr(&span, "error.type"),
      Some(Value::from("invalid_request"))
    );
    let _ = provider.shutdown();
  }

  #[test]
  fn with_retry_records_attempts_as_events_on_one_span() {
    let (exporter, provider, hook) = setup();
    let (stub, seen) = Stub::new(vec![503, 503, 200]);
    let policy = RetryPolicy::new(id_effect::Schedule::recurs(2));
    let eff = TracedHttpClient::with_retry(stub, policy)
      .execute(HttpRequest::get("http://svc/flaky"))
      .with_span_hook(hook);
    assert!(matches!(eff.block_on(), Exit::Success(r) if r.status == 200));

    let spans = exporter.get_finished_spans().expect("spans");
    assert_eq!(spans.len(), 1, "one span for the whole call");
    let span = &spans[0];
    assert_eq!(span.status, Status::Ok);
    let events: Vec<_> = span.events.iter().collect();
    assert_eq!(events.len(), 3);
    for (i, event) in events.iter().enumerate() {
      assert_eq!(event.name, "http.client.attempt");
      let resend = event
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "http.request.resend_count")
        .map(|kv| kv.value.clone());
      assert_eq!(resend, Some(Value::I64(i as i64)));
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert!(seen.iter().all(|h| traceparent(h) == traceparent(&seen[0])));
    let _ = provider.shutdown();
  }

  #[test]
  fn without_a_hook_the_ambient_context_is_injected() {
    install_w3c_propagators();
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let remote = Context::new().with_span(provider.tracer("ambient").start("ambient"));
    let (stub, seen) = Stub::new(vec![200]);
    let client = TracedHttpClient::from_service(HttpClientService::new(stub));
    {
      let _attached = remote.clone().attach();
      assert!(matches!(
        client.execute(HttpRequest::get("http://svc/")).block_on(),
        Exit::Success(_)
      ));
    }
    let sent = traceparent(&seen.lock().unwrap()[0]);
    assert!(sent.contains(&remote.span().span_context().span_id().to_string()));
    let _ = provider.shutdown();
  }

  #[test]
  fn unparseable_urls_keep_only_the_method() {
    let mut req = HttpRequest::get("not a url");
    req.method = HttpMethod::Patch;
    assert_eq!(
      client_attributes(&req),
      vec![
        ("otel.kind", "client".to_owned()),
        ("http.request.method", "PATCH".to_owned()),
      ]
    );
    assert_eq!(method_name(&HttpMethod::Put), "PUT");
    assert_eq!(method_name(&HttpMethod::Delete), "DELETE");
  }
}
