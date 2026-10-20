//! Tracing helpers for RPC calls, on every transport.
//!
//! Spans use stable field names so an OpenTelemetry layer on the subscriber can map them to
//! semantic conventions without this crate depending on OTEL crates directly.
//!
//! Every call opens one `rpc.request` span with `rpc.system` (`id_effect_rpc`), `rpc.service` and
//! `rpc.method` (the tag split at its last `.`), `rpc.operation` (the tag), `rpc.transport` and
//! `correlation.id`:
//!
//! - over HTTP, [`rpc_request_span`] per request (filled by [`record_request_metadata`]);
//! - over WebSocket and WebTransport, [`rpc_call_span`] per call, a child of the connection's
//!   [`rpc_connection_span`] (`rpc.connection`);
//! - over SSE, [`rpc_call_span`] per stream, a child of the `GET`'s HTTP `rpc.request` span;
//! - over the mesh, the endpoint's own `rpc.request` span (`transport::mesh`).

use tracing::Span;

/// `rpc.system` of every span this crate opens.
pub const RPC_SYSTEM: &str = "id_effect_rpc";

/// Root span for one RPC-style HTTP request. Record metadata with [`record_request_metadata`].
#[inline]
pub fn rpc_request_span() -> Span {
  tracing::info_span!(
    "rpc.request",
    rpc.system = RPC_SYSTEM,
    rpc.transport = "http",
    correlation.id = tracing::field::Empty,
    http.method = tracing::field::Empty,
    http.route = tracing::field::Empty,
    rpc.operation = tracing::field::Empty,
    rpc.service = tracing::field::Empty,
    rpc.method = tracing::field::Empty,
  )
}

/// Fill standard RPC span fields (safe for high-cardinality: avoid raw URLs with query tokens).
///
/// An `operation` naming a procedure also fills `rpc.service` and `rpc.method`; the route-level
/// operations (`rpc.dispatch`, `rpc.batch`: any `rpc.` name) do not.
#[inline]
pub fn record_request_metadata(
  span: &Span,
  correlation_id: Option<&str>,
  method: &str,
  route: &str,
  operation: &str,
) {
  if let Some(c) = correlation_id {
    span.record("correlation.id", c);
  }
  span.record("http.method", method);
  span.record("http.route", route);
  span.record("rpc.operation", operation);
  if !operation.starts_with("rpc.") {
    let (service, procedure) = split_tag(operation);
    span.record("rpc.service", service);
    span.record("rpc.method", procedure);
  }
}

/// The span of one connection carrying many calls (`transport`: `websocket`, `webtransport`).
#[inline]
pub fn rpc_connection_span(transport: &'static str, correlation_id: &str) -> Span {
  tracing::info_span!(
    "rpc.connection",
    rpc.system = RPC_SYSTEM,
    rpc.transport = transport,
    correlation.id = correlation_id,
  )
}

/// The `rpc.request` span of one call on `transport`, a child of `parent` (the connection's span),
/// or of the current span when `None` (the HTTP request carrying an SSE stream).
#[inline]
pub fn rpc_call_span(
  parent: Option<&Span>,
  transport: &'static str,
  tag: &str,
  correlation_id: &str,
) -> Span {
  let (service, method) = split_tag(tag);
  match parent {
    Some(parent) => tracing::info_span!(
      parent: parent,
      "rpc.request",
      rpc.system = RPC_SYSTEM,
      rpc.service = service,
      rpc.method = method,
      rpc.operation = tag,
      rpc.transport = transport,
      correlation.id = correlation_id,
    ),
    None => tracing::info_span!(
      "rpc.request",
      rpc.system = RPC_SYSTEM,
      rpc.service = service,
      rpc.method = method,
      rpc.operation = tag,
      rpc.transport = transport,
      correlation.id = correlation_id,
    ),
  }
}

/// `Service.method` as (`Service`, `method`); a tag without a `.` is a method of no service.
fn split_tag(tag: &str) -> (&str, &str) {
  tag.rsplit_once('.').unwrap_or(("", tag))
}

#[cfg(test)]
mod tests {
  use super::*;

  mod rpc_request_span {
    use super::*;

    #[test]
    fn constructs_without_panicking() {
      let span = rpc_request_span();
      let _g = span.enter();
      record_request_metadata(&span, Some("c1"), "POST", "/greet", "greet");
    }

    #[test]
    fn allows_missing_correlation_id() {
      let span = rpc_request_span();
      let _g = span.enter();
      record_request_metadata(&span, None, "GET", "/health", "health");
    }
  }

  mod split_tag {
    use super::*;

    #[test]
    fn splits_at_the_last_dot() {
      assert_eq!(split_tag("Shop.Cart.add"), ("Shop.Cart", "add"));
      assert_eq!(split_tag("greet"), ("", "greet"));
    }
  }

  mod rpc_call_span {
    use super::*;

    #[test]
    fn constructs_under_a_connection() {
      let conn = rpc_connection_span("websocket", "c1");
      let span = rpc_call_span(Some(&conn), "websocket", "Greeter.greet", "c1-1");
      let _g = span.enter();
      let _ = rpc_call_span(None, "sse", "Greeter.watch", "c2");
    }
  }
}
