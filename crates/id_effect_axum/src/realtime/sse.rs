//! Server-sent events (feature `sse`; ADR 0014 E12.2, web.realtime #2 #8).
//!
//! - [`SseEvent`]: one event (`id`, `event`, `data`, `retry`), encoded per the HTML living
//!   standard; ids and event names never carry a line break (they are dropped from the field),
//!   multi-line data becomes several `data:` lines.
//! - [`LastEventId`]: the `Last-Event-ID` request header a reconnecting `EventSource` sends,
//!   validated (at most [`MAX_LAST_EVENT_ID_BYTES`] bytes of UTF-8 without CR, LF or NUL; a bad
//!   one answers **400** Problem Details). The handler resumes after it.
//! - [`encode`] turns a `Stream<SseEvent, E>` into the response bytes with a heartbeat comment
//!   every [`SseConfig::keep_alive`] on the executor clock; [`response`] wraps it in a
//!   `text/event-stream` response, and dropping the response (client gone) interrupts the
//!   producer, finalizers included.
//! - [`endpoint`]: a typed `GET` route (decide `.public()` / `.guard(g)`), counted against a
//!   per-IP and total [`ConnectionCap`].
//!
//! ```rust
//! use id_effect::{Effect, Never, Stream, succeed};
//! use id_effect_axum::RequestCaps;
//! use id_effect_axum::realtime::sse::{self, SseConfig, SseEvent, SseRequest};
//! use id_effect_axum::route::Router;
//!
//! /// Counts from just after the client's last event id.
//! fn ticker(_: &mut (), _: RequestCaps, req: SseRequest) -> Effect<Stream<SseEvent>, Never, ()> {
//!   let from = req.last_event_id.and_then(|id| id.as_u64()).map_or(0, |n| n + 1);
//!   succeed(Stream::from_iterable(from..from + 3).map(|n| SseEvent::data(n.to_string()).id(n.to_string())))
//! }
//!
//! let app: axum::Router = Router::new()
//!   .route(sse::endpoint("/ticks", SseConfig::default(), ticker).public())
//!   .with_state(())
//!   .into_axum();
//! # let _ = app;
//! ```

use std::fmt;
use std::future::poll_fn;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect::{Ctx, Effect, Exit, HNil, Never, Stream, die, fail, from_async, succeed, suspend};
use tokio::sync::mpsc;

use super::ConnectionCap;
use crate::body::{BoxError, stream_body};
use crate::client_ip::{ClientIp, TrustedProxies};
use crate::request_caps::RequestCaps;
use crate::route::{Endpoint, Problem, RouteInfo, Undecided};

/// `Last-Event-ID`.
pub const LAST_EVENT_ID: HeaderName = HeaderName::from_static("last-event-id");
/// The longest `Last-Event-ID` accepted, in bytes.
pub const MAX_LAST_EVENT_ID_BYTES: usize = 256;
/// The default heartbeat interval: 15 s.
pub const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(15);

/// One server-sent event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
  id: Option<String>,
  event: Option<String>,
  data: String,
  retry: Option<Duration>,
}

/// Drops the characters a field value may not hold (CR, LF, NUL).
fn field(value: impl Into<String>) -> String {
  let mut value = value.into();
  value.retain(|c| !matches!(c, '\r' | '\n' | '\0'));
  value
}

impl SseEvent {
  /// An event with `data` (several lines become several `data:` lines).
  pub fn data(data: impl Into<String>) -> Self {
    Self {
      data: data.into(),
      ..Self::default()
    }
  }

  /// An event whose data is `value` as JSON.
  ///
  /// # Errors
  ///
  /// When `value` does not serialize.
  pub fn json<T: serde::Serialize>(value: &T) -> Result<Self, serde_json::Error> {
    serde_json::to_string(value).map(Self::data)
  }

  /// The event id the client echoes as `Last-Event-ID` when it reconnects (CR, LF and NUL are
  /// dropped).
  #[must_use]
  pub fn id(mut self, id: impl Into<String>) -> Self {
    self.id = Some(field(id));
    self
  }

  /// The event type (`addEventListener(name, …)`; CR, LF and NUL are dropped).
  #[must_use]
  pub fn event(mut self, name: impl Into<String>) -> Self {
    self.event = Some(field(name));
    self
  }

  /// The reconnection delay the client should use.
  #[must_use]
  pub fn retry(mut self, delay: Duration) -> Self {
    self.retry = Some(delay);
    self
  }

  /// The event id, if set.
  pub fn event_id(&self) -> Option<&str> {
    self.id.as_deref()
  }

  /// The wire form, ending with the blank line.
  pub fn encode(&self) -> Bytes {
    let mut out = String::with_capacity(self.data.len() + 32);
    if let Some(name) = &self.event {
      out.push_str("event: ");
      out.push_str(name);
      out.push('\n');
    }
    for line in self.data.split('\n') {
      out.push_str("data: ");
      out.push_str(line.strip_suffix('\r').unwrap_or(line));
      out.push('\n');
    }
    if let Some(id) = &self.id {
      out.push_str("id: ");
      out.push_str(id);
      out.push('\n');
    }
    if let Some(retry) = self.retry {
      out.push_str(&format!("retry: {}\n", retry.as_millis()));
    }
    out.push('\n');
    Bytes::from(out)
  }
}

/// A validated `Last-Event-ID`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LastEventId(String);

/// A `Last-Event-ID` header that is too long, not UTF-8, or holds CR, LF or NUL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidLastEventId;

impl fmt::Display for InvalidLastEventId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "Last-Event-ID must be at most {MAX_LAST_EVENT_ID_BYTES} bytes of UTF-8 without CR, LF or NUL"
    )
  }
}

impl std::error::Error for InvalidLastEventId {}

impl IntoResponse for InvalidLastEventId {
  fn into_response(self) -> Response {
    Problem::new(StatusCode::BAD_REQUEST)
      .with_title("Invalid Last-Event-ID")
      .with_detail(self.to_string())
      .into_response()
  }
}

impl LastEventId {
  /// Decodes a header value's bytes (the decoder the fuzz target drives).
  ///
  /// # Errors
  ///
  /// [`InvalidLastEventId`] (see the type).
  pub fn parse(bytes: &[u8]) -> Result<Self, InvalidLastEventId> {
    if bytes.len() > MAX_LAST_EVENT_ID_BYTES || bytes.iter().any(|b| matches!(b, b'\r' | b'\n' | 0))
    {
      return Err(InvalidLastEventId);
    }
    std::str::from_utf8(bytes)
      .map(|s| Self(s.to_owned()))
      .map_err(|_| InvalidLastEventId)
  }

  /// The id of the request's `Last-Event-ID` header: `None` without one (or an empty one, which
  /// resets the id).
  ///
  /// # Errors
  ///
  /// [`InvalidLastEventId`] for a malformed header or more than one.
  pub fn from_headers(headers: &axum::http::HeaderMap) -> Result<Option<Self>, InvalidLastEventId> {
    let mut values = headers.get_all(LAST_EVENT_ID).iter();
    let Some(value) = values.next() else {
      return Ok(None);
    };
    if values.next().is_some() {
      return Err(InvalidLastEventId);
    }
    let id = Self::parse(value.as_bytes())?;
    Ok((!id.0.is_empty()).then_some(id))
  }

  /// The id as sent.
  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// The id as a number, when it is one (the usual sequence-number ids).
  pub fn as_u64(&self) -> Option<u64> {
    self.0.parse().ok()
  }
}

impl fmt::Display for LastEventId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// Stream settings and limits of an SSE endpoint; [`Default`] is the secure default.
#[derive(Clone, Debug)]
pub struct SseConfig {
  keep_alive: Duration,
  retry: Option<Duration>,
  buffer: usize,
  max_streams: usize,
  max_streams_per_ip: usize,
  cap: Option<ConnectionCap>,
}

impl Default for SseConfig {
  fn default() -> Self {
    Self {
      keep_alive: DEFAULT_KEEP_ALIVE,
      retry: None,
      buffer: 16,
      max_streams: super::DEFAULT_MAX_CONNECTIONS,
      max_streams_per_ip: super::DEFAULT_MAX_CONNECTIONS_PER_IP,
      cap: None,
    }
  }
}

impl SseConfig {
  /// A heartbeat comment after `interval` (at least 1 ms) without an event, so proxies keep the
  /// stream open and a dead client is noticed on write.
  #[must_use]
  pub fn keep_alive(mut self, interval: Duration) -> Self {
    self.keep_alive = interval.max(Duration::from_millis(1));
    self
  }

  /// Send `retry: <delay>` first: the client's reconnection delay.
  #[must_use]
  pub fn retry(mut self, delay: Duration) -> Self {
    self.retry = Some(delay);
    self
  }

  /// Events buffered ahead of a slow client (at least 1); the producer waits beyond it.
  #[must_use]
  pub fn buffer(mut self, events: usize) -> Self {
    self.buffer = events.max(1);
    self
  }

  /// At most `total` open streams on the endpoint and `per_ip` from one client IP (each at
  /// least 1); beyond them **429**.
  #[must_use]
  pub fn max_streams(mut self, total: usize, per_ip: usize) -> Self {
    self.max_streams = total.max(1);
    self.max_streams_per_ip = per_ip.max(1);
    self.cap = None;
    self
  }

  /// Count streams against `cap`, shared with other endpoints.
  #[must_use]
  pub fn connection_cap(mut self, cap: ConnectionCap) -> Self {
    self.cap = Some(cap);
    self
  }

  /// The heartbeat interval.
  pub fn keep_alive_interval(&self) -> Duration {
    self.keep_alive
  }

  fn cap(&self) -> ConnectionCap {
    self
      .cap
      .clone()
      .unwrap_or_else(|| ConnectionCap::new(self.max_streams, self.max_streams_per_ip))
  }
}

/// What an SSE handler receives besides its state and the request caps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseRequest {
  /// The client's `Last-Event-ID`: resume after it.
  pub last_event_id: Option<LastEventId>,
  /// The client's address, when the server records connect info.
  pub client_ip: Option<IpAddr>,
}

/// One message from the event pump: an event, or how the event stream failed.
enum Pumped<E> {
  Event(SseEvent),
  Failed(E),
  Died(String),
}

/// A heartbeat comment.
const PING: &[u8] = b":\n\n";

/// The bytes of `events` as an event stream: `retry` first when configured, each event encoded,
/// and a heartbeat comment whenever [`SseConfig::keep_alive`] passes without an event (on the
/// executor clock of the pulling fiber).
///
/// The events are pulled by a child fiber of the first pulling fiber into a buffer of
/// [`SseConfig::buffer`] events, so the heartbeat runs while the producer waits; when the
/// consumer stops (or is interrupted) that fiber is interrupted and the producer's finalizers
/// run. A failure of `events` fails the byte stream after the events before it.
pub fn encode<E>(events: Stream<SseEvent, E>, config: &SseConfig) -> Stream<Bytes, E>
where
  E: Send + 'static,
{
  let (tx, rx) = mpsc::channel::<Pumped<E>>(config.buffer);
  let pump = events
    .run_for_each_effect({
      let tx = tx.clone();
      move |event| {
        let tx = tx.clone();
        from_async(move || async move {
          let _ = tx.send(Pumped::Event(event)).await;
          Ok(())
        })
      }
    })
    .exit::<Never>()
    .flat_map(move |exit| {
      let last = match exit {
        Exit::Success(()) => None,
        Exit::Failure(cause) => match crate::split_cause(cause) {
          Ok(e) => Some(Pumped::Failed(e)),
          Err(rest) => crate::first_defect(&rest).map(|m| Pumped::Died(m.to_owned())),
        },
      };
      from_async(move || async move {
        if let Some(last) = last {
          let _ = tx.send(last).await;
        }
        Ok(())
      })
    });
  let state = Arc::new(Mutex::new((Some(pump), config.retry)));
  let rx = Arc::new(tokio::sync::Mutex::new(rx));
  let keep_alive = config.keep_alive;
  Stream::from_pull(move |_max| {
    let (pump, retry) = {
      let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
      (st.0.take(), st.1.take())
    };
    let rx = Arc::clone(&rx);
    let start: Effect<(), E, HNil> = match pump {
      Some(pump) => pump.fork().map(drop).widen(),
      None => succeed(()),
    };
    start.flat_map(move |()| match retry {
      Some(retry) => succeed(Some(vec![Bytes::from(format!(
        "retry: {}\n\n",
        retry.as_millis()
      ))])),
      None => next(rx, keep_alive),
    })
  })
}

/// The next chunk of the byte stream: an event, a heartbeat after `keep_alive`, or the end.
fn next<E>(
  rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Pumped<E>>>>,
  keep_alive: Duration,
) -> Effect<Option<Vec<Bytes>>, E, HNil>
where
  E: Send + 'static,
{
  Effect::from_ctx(move |ctx: Ctx<HNil>| {
    let exec = Arc::clone(ctx.fiber().executor());
    async move {
      let mut rx = rx.lock().await;
      let mut timer = exec.sleep(keep_alive);
      let got = poll_fn(|cx| {
        if let Poll::Ready(got) = rx.poll_recv(cx) {
          return Poll::Ready(Some(got));
        }
        timer.as_mut().poll(cx).map(|()| None)
      })
      .await;
      Ok::<_, Never>(got)
    }
  })
  .widen()
  .flat_map(|got| match got {
    None => succeed(Some(vec![Bytes::from_static(PING)])),
    Some(None) => succeed(None),
    Some(Some(Pumped::Event(event))) => succeed(Some(vec![event.encode()])),
    Some(Some(Pumped::Failed(e))) => fail(e),
    Some(Some(Pumped::Died(message))) => die(format!("event stream defect: {message}")),
  })
}

/// A `text/event-stream` response streaming `events` (see [`encode`]); dropping it (the client
/// went away) interrupts the producer.
pub fn response<E>(events: Stream<SseEvent, E>, config: &SseConfig) -> Response
where
  E: Into<BoxError> + Send + 'static,
{
  let mut res = Response::new(stream_body(encode(events, config)));
  let headers = res.headers_mut();
  headers.insert(
    header::CONTENT_TYPE,
    HeaderValue::from_static("text/event-stream"),
  );
  headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
  headers.insert(
    HeaderName::from_static("x-accel-buffering"),
    HeaderValue::from_static("no"),
  );
  res
}

/// A typed `GET` route at `path` streaming the events `handler` returns (see the
/// [module docs](self)). Decide its access with `.public()` or `.guard(g)`.
///
/// Per request: a malformed `Last-Event-ID` answers **400**; beyond the stream caps **429**;
/// then `handler(&mut state, caps, request)` runs as a request fiber, a typed failure answers
/// its own response, and its stream is sent as events.
pub fn endpoint<S, E, E2, F>(
  path: impl Into<String>,
  config: SseConfig,
  handler: F,
) -> Endpoint<S, Undecided>
where
  S: Clone + Send + Sync + 'static,
  E: IntoResponse + Send + 'static,
  E2: Into<BoxError> + Send + 'static,
  F: Fn(&mut S, RequestCaps, SseRequest) -> Effect<Stream<SseEvent, E2>, E, S>
    + Clone
    + Send
    + Sync
    + 'static,
{
  let cap = config.cap();
  let config = Arc::new(config);
  let mut info = RouteInfo::new(Method::GET, path);
  info.handler = Some(std::any::type_name::<F>());
  let method_router = axum::routing::get(move |State(env): State<S>, req: Request| {
    let (cap, config, f) = (cap.clone(), Arc::clone(&config), handler.clone());
    async move {
      let last_event_id = match LastEventId::from_headers(req.headers()) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
      };
      let none = TrustedProxies::none();
      let trusted = req.extensions().get::<TrustedProxies>().unwrap_or(&none);
      let client_ip = ClientIp::from_parts(req.extensions(), req.headers(), trusted).map(|c| c.0);
      let permit = match cap.acquire(client_ip) {
        Ok(permit) => permit,
        Err(reached) => return reached.into_response(),
      };
      let caps = req
        .extensions()
        .get::<RequestCaps>()
        .cloned()
        .unwrap_or_default();
      let request = SseRequest {
        last_event_id,
        client_ip,
      };
      match crate::try_run_effect_from_state(env, move |s| f(s, caps, request)).await {
        Ok(Exit::Success(events)) => {
          // The permit lives as long as the stream does.
          let events = events.chain(Stream::from_effect(suspend(move || {
            drop(permit);
            succeed(Vec::new())
          })));
          response(events, &config)
        }
        Ok(Exit::Failure(cause)) => crate::exit_response::<(), E>(Exit::Failure(cause)),
        Err(saturated) => saturated.into_response(),
      }
    }
  });
  Endpoint::from_parts(info, method_router)
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::http::HeaderMap;

  #[test]
  fn events_encode_fields_and_multiline_data() {
    let e = SseEvent::data("a\nb\r\nc")
      .id("4\n2")
      .event("tick\r")
      .retry(Duration::from_millis(1500));
    assert_eq!(
      e.encode(),
      "event: tick\ndata: a\ndata: b\ndata: c\nid: 42\nretry: 1500\n\n"
    );
    assert_eq!(e.event_id(), Some("42"));
    assert_eq!(SseEvent::data("").encode(), "data: \n\n");
    assert_eq!(
      SseEvent::json(&serde_json::json!({"n": 1}))
        .unwrap()
        .encode(),
      "data: {\"n\":1}\n\n"
    );
  }

  #[test]
  fn last_event_id_is_validated() {
    assert_eq!(LastEventId::parse(b"17").unwrap().as_u64(), Some(17));
    assert_eq!(LastEventId::parse(b"abc").unwrap().as_str(), "abc");
    assert_eq!(LastEventId::parse(b"abc").unwrap().to_string(), "abc");
    assert_eq!(LastEventId::parse(b"abc").unwrap().as_u64(), None);
    for bad in [&b"a\nb"[..], b"a\rb", b"a\0b", b"\xff", &[b'1'; 257]] {
      assert_eq!(LastEventId::parse(bad), Err(InvalidLastEventId));
    }
    assert!(LastEventId::parse(&[b'1'; 256]).is_ok());
    let mut h = HeaderMap::new();
    assert_eq!(LastEventId::from_headers(&h), Ok(None));
    h.insert(LAST_EVENT_ID, HeaderValue::from_static(""));
    assert_eq!(LastEventId::from_headers(&h), Ok(None));
    h.insert(LAST_EVENT_ID, HeaderValue::from_static("9"));
    assert_eq!(
      LastEventId::from_headers(&h).unwrap().unwrap().as_u64(),
      Some(9)
    );
    h.append(LAST_EVENT_ID, HeaderValue::from_static("10"));
    assert_eq!(LastEventId::from_headers(&h), Err(InvalidLastEventId));
    let res = InvalidLastEventId.into_response();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
  }

  #[test]
  fn config_clamps() {
    let c = SseConfig::default()
      .keep_alive(Duration::ZERO)
      .buffer(0)
      .max_streams(0, 0);
    assert_eq!(c.keep_alive_interval(), Duration::from_millis(1));
    assert_eq!(c.buffer, 1);
    assert_eq!(c.cap().max_total(), 1);
    let shared = ConnectionCap::new(5, 2);
    assert_eq!(c.connection_cap(shared).cap().max_per_ip(), 2);
    assert_eq!(
      SseConfig::default().keep_alive_interval(),
      DEFAULT_KEEP_ALIVE
    );
  }
}
