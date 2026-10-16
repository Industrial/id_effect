//! [`remote_parent`]: the W3C trace context a request carries.

use axum::extract::Request;
use id_effect_opentelemetry::extract_trace_context_from_headers;
use opentelemetry::Context;

/// The W3C trace context (`traceparent` / `tracestate`) of `request`'s headers, through the
/// globally installed propagator; an empty context when there is none. Headers that are not
/// visible ASCII are skipped.
pub(crate) fn remote_parent(request: &Request) -> Context {
  let headers: Vec<(String, String)> = request
    .headers()
    .iter()
    .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
    .collect();
  extract_trace_context_from_headers(&Context::new(), &headers)
}

#[cfg(test)]
mod tests {
  use axum::body::Body;
  use id_effect_opentelemetry::install_w3c_propagators;
  use opentelemetry::trace::TraceContextExt as _;

  use super::*;

  fn request(traceparent: Option<&str>) -> Request {
    let mut builder = axum::http::Request::builder().uri("/");
    if let Some(value) = traceparent {
      builder = builder.header("traceparent", value);
    }
    builder.body(Body::empty()).unwrap()
  }

  #[test]
  fn reads_a_traceparent_header() {
    install_w3c_propagators();
    let parent = remote_parent(&request(Some(
      "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
    )));
    let span = parent.span();
    let ctx = span.span_context();
    assert!(ctx.is_valid() && ctx.is_remote());
    assert_eq!(
      ctx.trace_id().to_string(),
      "4bf92f3577b34da6a3ce929d0e0e4736"
    );
  }

  #[test]
  fn without_a_traceparent_there_is_no_parent() {
    install_w3c_propagators();
    assert!(
      !remote_parent(&request(None))
        .span()
        .span_context()
        .is_valid()
    );
  }
}
