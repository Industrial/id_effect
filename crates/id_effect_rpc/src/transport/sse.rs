//! Subscriptions and server streams as server-sent events (feature `sse`; design note §2): the
//! browser fallback next to the HTTP routes, so mutations keep going over `POST`.
//!
//! `GET {path}/{tag}?input=<url-encoded JSON>` with `Accept: text/event-stream` on a subscription
//! or a server stream answers `text/event-stream`, one event per frame:
//!
//! ```text
//! event: item
//! data: {"text":"hi"}
//! id: c1.AQAAAAAAAAAH
//!
//! event: exit
//! data: {"_tag":"Success","value":null}
//! ```
//!
//! - `id:` is the item's [`Cursor`] when it has one, so a reconnecting `EventSource` sends it back
//!   as `Last-Event-ID` and a subscription resumes after it. The header is validated by
//!   [`LastEventId`] (**400** when malformed) and must decode as a cursor (**400** otherwise); a
//!   server stream ignores it (it does not resume).
//! - After the `exit` event the response ends; a browser client closes its `EventSource` on
//!   `exit`, or it reconnects and calls again.
//! - The procedure's guards run on the `GET` request, as for a query; refusals and unknown tags are
//!   Problems before the stream starts. Like queries on `GET`, a procedure served this way must
//!   not change state: a cross-site page can start the request, though it cannot read it.
//! - The stream is pull-based: the realtime layer buffers [`SseConfig::buffer`] events ahead of a
//!   slow client and the call's window behind them, then the producer waits. A disconnect
//!   interrupts the call (finalizers run). There is no deadline: the stream runs until it ends.
//! - The connection caps (16 per client IP, 10,000 per server: **429**) and the 15 s heartbeat
//!   comment are the realtime layer's ([`RpcServer::with_sse`]).

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::handler::Handler as _;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use futures_util::StreamExt as _;
use id_effect::{DynEnv, Effect, Stream, fail, from_async, succeed};
use id_effect_axum::RequestCaps;
use id_effect_axum::realtime::sse::{
  self as realtime, LastEventId, SseConfig, SseEvent, SseRequest,
};
use id_effect_axum::route::Problem;
use serde_json::Value;

use crate::cancel::CallControl;
use crate::contract::ProcedureKind;
use crate::handler::CallContext;
use crate::kinds::CallFrames;
use crate::server::RpcServer;
use crate::server::dispatch;
use crate::wire::{CallId, Cursor, Frame, FrameKind};

/// The media type of an event stream.
pub const EVENT_STREAM: &str = "text/event-stream";

/// Whether the request asks for an event stream (`Accept: text/event-stream`).
pub fn accepts_event_stream(headers: &HeaderMap) -> bool {
  headers
    .get_all(header::ACCEPT)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|v| v.split(','))
    .any(|t| {
      let essence = t.split(';').next().unwrap_or_default().trim();
      essence.eq_ignore_ascii_case(EVENT_STREAM)
    })
}

/// The realtime layer's SSE endpoint, called from the `GET {path}/{tag}` route (its connection
/// cap is shared by every clone).
#[derive(Clone)]
pub(crate) struct SseMount {
  router: MethodRouter<DynEnv>,
}

impl SseMount {
  pub(crate) fn new(config: SseConfig) -> Self {
    let (_, router) = realtime::endpoint("/", config, open).into_parts();
    Self { router }
  }
}

/// What the `GET {path}/{tag}` route resolved and authorized, for the event stream.
struct Target {
  server: RpcServer,
  kind: ProcedureKind,
  tag: String,
  version: Option<u32>,
  payload: Value,
  correlation_id: Arc<str>,
}

/// Serve the call as an event stream (the route already resolved `tag` and ran its guards).
pub(crate) async fn serve(
  server: &RpcServer,
  ctx: CallContext,
  parts: Parts,
  kind: ProcedureKind,
  tag: String,
  version: Option<u32>,
  payload: Value,
) -> Response {
  let mut caps = parts
    .extensions
    .get::<RequestCaps>()
    .cloned()
    .unwrap_or_default();
  caps.insert(Target {
    server: server.clone(),
    kind,
    tag,
    version,
    payload,
    correlation_id: ctx.correlation_id,
  });
  let mut request = Request::from_parts(parts, Body::empty());
  request.extensions_mut().insert(caps);
  server
    .sse_mount()
    .router
    .clone()
    .call(request, ctx.env)
    .await
}

fn bad_request(detail: &str) -> Response {
  Problem::new(StatusCode::BAD_REQUEST)
    .with_detail(detail)
    .into_response()
}

/// The realtime endpoint's handler: start the call, its frames become events.
fn open(
  env: &mut DynEnv,
  caps: RequestCaps,
  request: SseRequest,
) -> Effect<Stream<SseEvent, std::io::Error>, Response, DynEnv> {
  let Some(target) = caps.get::<Target>() else {
    return fail(bad_request("not an rpc stream"));
  };
  let after = match (&target.kind, &request.last_event_id) {
    (ProcedureKind::Subscription, Some(id)) => match Cursor::decode(id.as_str()) {
      Ok(cursor) => Some(cursor),
      Err(_) => return fail(bad_request("Last-Event-ID is not an rpc cursor")),
    },
    _ => None,
  };
  // The request's capabilities (an `Authenticate` layer's principal) and this stream's own
  // connection id.
  let mut env = caps.overlay(env);
  env.insert_arc(Arc::new(super::ConnectionId::fresh()));
  let ctx = CallContext::new(env, Arc::clone(&target.correlation_id));
  let started =
    dispatch::resolve(target.server.group(), &target.tag, target.version).and_then(|entry| {
      let id = CallId::new(1).unwrap_or(CallId::CONNECTION);
      let (payload, window) = (target.payload.clone(), target.server.stream_window());
      let control = CallControl::none();
      match target.kind {
        ProcedureKind::Subscription => {
          dispatch::start_subscription(entry, id, payload, after, ctx, control, window)
        }
        _ => dispatch::start_server_stream(entry, id, payload, ctx, control, window),
      }
    });
  match started {
    Ok(frames) => succeed(events(frames)),
    Err(rejection) => fail(bad_request(&format!("{rejection:?}"))),
  }
}

/// The frames of a call as events: `item`s, then the `exit`.
fn events(frames: CallFrames) -> Stream<SseEvent, std::io::Error> {
  let frames = Arc::new(tokio::sync::Mutex::new(frames));
  Stream::from_pull(move |_max| {
    let frames = Arc::clone(&frames);
    from_async(move || async move {
      let next = frames.lock().await.next().await;
      Ok(next.map(|frame| vec![event(&frame)]))
    })
  })
}

/// One frame as an event (`event: item` with the cursor as `id:`, or `event: exit`).
pub fn event(frame: &Frame) -> SseEvent {
  let data =
    serde_json::to_string(frame.payload.as_ref().unwrap_or(&Value::Null)).unwrap_or_default();
  let name = if frame.kind == FrameKind::Exit {
    "exit"
  } else {
    "item"
  };
  let event = SseEvent::data(data).event(name);
  match frame.cursor {
    Some(cursor) => event.id(cursor.to_string()),
    None => event,
  }
}

/// The validated `Last-Event-ID` of `headers` as a cursor (`Ok(None)` without one).
///
/// # Errors
///
/// A malformed header, or one that is not a cursor.
pub fn last_cursor(headers: &HeaderMap) -> Result<Option<Cursor>, String> {
  match LastEventId::from_headers(headers) {
    Err(err) => Err(err.to_string()),
    Ok(None) => Ok(None),
    Ok(Some(id)) => Cursor::decode(id.as_str())
      .map(Some)
      .map_err(|err| err.to_string()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::http::HeaderValue;

  #[test]
  fn accept_headers_pick_event_streams() {
    let mut headers = HeaderMap::new();
    assert!(!accepts_event_stream(&headers));
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    assert!(!accepts_event_stream(&headers));
    headers.insert(
      header::ACCEPT,
      HeaderValue::from_static("application/json, Text/Event-Stream; q=0.9"),
    );
    assert!(accepts_event_stream(&headers));
  }

  #[test]
  fn frames_become_events() {
    let id = CallId::new(1).unwrap();
    let item = Frame::new(id, FrameKind::Item)
      .with_cursor(Cursor::events(7))
      .with_payload(serde_json::json!({"a": 1}));
    let text = String::from_utf8(event(&item).encode().to_vec()).unwrap();
    assert_eq!(
      text,
      format!(
        "event: item\ndata: {{\"a\":1}}\nid: {}\n\n",
        Cursor::events(7)
      )
    );
    let exit = Frame::new(id, FrameKind::Exit);
    assert_eq!(event(&exit).encode(), &b"event: exit\ndata: null\n\n"[..]);
  }

  #[test]
  fn last_event_ids_are_cursors() {
    let mut headers = HeaderMap::new();
    assert_eq!(last_cursor(&headers), Ok(None));
    let cursor = Cursor::events(3);
    headers.insert("last-event-id", cursor.to_string().parse().unwrap());
    assert_eq!(last_cursor(&headers), Ok(Some(cursor)));
    headers.insert("last-event-id", HeaderValue::from_static("42"));
    assert!(last_cursor(&headers).is_err());
    headers.insert("last-event-id", HeaderValue::from_bytes(&[0xff]).unwrap());
    assert!(last_cursor(&headers).is_err());
  }
}
