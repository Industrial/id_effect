//! The server-sent-events client (feature `sse`): subscriptions and server streams as
//! `GET {url}/{tag}?input=…` with `Accept: text/event-stream`, through id_effect_platform's
//! `HttpClientService` (its SSRF guard, timeouts and body cap apply).
//!
//! [`RpcClient::sse`] answers an [`SseCall`] whose [`next`](SseCall::next) yields the events as
//! [`CallEvent`]s; [`SseCall::last_cursor`] is where to resume a subscription after a disconnect
//! (sent as `Last-Event-ID`). A refusal before the stream starts is [`RpcClientError::Refused`]
//! with the server's Problem. The platform client's total timeout (30 s by default) bounds the
//! response: for long subscriptions configure a longer one, and resume after it.

use std::sync::{Arc, Mutex, PoisonError};

use id_effect::{Effect, HList, Selector, fail, need, succeed, suspend};
use id_effect_axum::route::Problem;
use id_effect_platform::http::{HttpBody, HttpClientService, HttpMethod, HttpRequest};
use serde_json::Value;

use super::{CallEvent, RPC_HEADER, RpcClient, RpcClientError, exit_result};
use crate::service::{
  BoxFuture, FrameStream, HttpTransport, RpcTransport, TransportCall, TransportError,
};
use crate::wire::{CallId, Cursor, Frame, FrameKind, MAX_FRAME_BYTES, WireExit};
use id_effect::Exit;
use id_effect_axum::EffectExecutor;

/// The most bytes of one event the parser buffers.
const MAX_EVENT_BYTES: usize = 2 * MAX_FRAME_BYTES;

/// Percent-encode `text` for a query string (RFC 3986 unreserved characters pass).
fn percent_encode(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  for b in text.bytes() {
    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
      out.push(char::from(b));
    } else {
      out.push_str(&format!("%{b:02X}"));
    }
  }
  out
}

impl RpcClient {
  /// Open the subscription or server stream `tag` with `input` as server-sent events, resuming
  /// a subscription after `after` (`Last-Event-ID`).
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Http`] (transport), [`RpcClientError::Refused`] (a Problem: unknown tag,
  /// guard, malformed cursor, connection cap) or [`RpcClientError::UnexpectedStatus`].
  pub fn sse<R, I>(
    &self,
    tag: &str,
    input: &Value,
    after: Option<Cursor>,
  ) -> Effect<SseCall, RpcClientError, R>
  where
    R: Selector<HttpClientService, I> + HList,
  {
    self.sse_version(tag, None, input, after)
  }

  /// [`RpcClient::sse`] at service version `v` (`x-rpc-version`; `None`: the newest served).
  ///
  /// # Errors
  ///
  /// As [`RpcClient::sse`].
  pub fn sse_version<R, I>(
    &self,
    tag: &str,
    v: Option<u32>,
    input: &Value,
    after: Option<Cursor>,
  ) -> Effect<SseCall, RpcClientError, R>
  where
    R: Selector<HttpClientService, I> + HList,
  {
    let url = format!(
      "{}?input={}",
      self.config().tag_url(tag),
      percent_encode(&input.to_string())
    );
    let mut headers = vec![
      ("accept".to_owned(), "text/event-stream".to_owned()),
      (RPC_HEADER.to_owned(), "1".to_owned()),
    ];
    if let Some(cursor) = after {
      headers.push(("last-event-id".to_owned(), cursor.to_string()));
    }
    if let Some(v) = v {
      headers.push((crate::server::VERSION_HEADER.to_owned(), v.to_string()));
    }
    let req = HttpRequest {
      method: HttpMethod::Get,
      url,
      headers,
      body: None,
      timeout: None,
      max_body_bytes: Some(usize::MAX),
    };
    need::<HttpClientService>().flat_map(move |client| {
      client
        .execute_stream(req)
        .map_err(RpcClientError::Http)
        .flat_map(move |resp| {
          if resp.status == 200 {
            return succeed(SseCall::new(resp.body, after));
          }
          let status = resp.status;
          resp
            .body
            .collect()
            .map_err(RpcClientError::Http)
            .flat_map(move |body| match serde_json::from_slice::<Problem>(&body) {
              Ok(problem) => fail(RpcClientError::Refused(Box::new(problem))),
              Err(_) => fail(RpcClientError::UnexpectedStatus(status)),
            })
        })
        .narrow()
    })
  }
}

/// Splits an event stream into events (the WHATWG rules the RPC server's events need: `event`,
/// `data`, `id` fields, `:` comments, LF or CRLF line ends).
#[derive(Debug, Default)]
struct Parser {
  buffer: Vec<u8>,
}

/// One parsed event.
#[derive(Debug, Default, PartialEq, Eq)]
struct Event {
  name: String,
  data: String,
  id: Option<String>,
}

impl Parser {
  fn feed(&mut self, bytes: &[u8]) {
    self.buffer.extend_from_slice(bytes);
  }

  /// The next complete event (`Ok(None)`: more bytes needed).
  fn next(&mut self) -> Result<Option<Event>, RpcClientError> {
    loop {
      let text = &self.buffer;
      let end = text
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|i| (i, 2))
        .into_iter()
        .chain(
          text
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| (i, 4)),
        )
        .min_by_key(|(i, _)| *i);
      let Some((at, sep)) = end else {
        if self.buffer.len() > MAX_EVENT_BYTES {
          return Err(RpcClientError::Transport(format!(
            "an event over {MAX_EVENT_BYTES} bytes"
          )));
        }
        return Ok(None);
      };
      let block: Vec<u8> = self.buffer.drain(..at + sep).collect();
      let block = String::from_utf8_lossy(&block[..at]).into_owned();
      let mut event = Event::default();
      let mut data = Vec::new();
      for line in block.lines() {
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
          "event" => event.name = value.to_owned(),
          "data" => data.push(value),
          "id" => event.id = Some(value.to_owned()),
          _ => {}
        }
      }
      if data.is_empty() {
        // A comment (heartbeat) or a field-only block: no event.
        continue;
      }
      event.data = data.join("\n");
      return Ok(Some(event));
    }
  }
}

#[derive(Debug)]
struct State {
  parser: Parser,
  eof: bool,
  done: bool,
  last: Option<Cursor>,
}

/// An open event stream ([`RpcClient::sse`]). Cheap to clone; clones read the same stream.
#[derive(Clone)]
pub struct SseCall {
  body: Arc<HttpBody>,
  state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for SseCall {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SseCall")
      .field("last_cursor", &self.last_cursor())
      .finish_non_exhaustive()
  }
}

/// What the buffered bytes give.
enum Step {
  Event(CallEvent),
  End,
  More,
}

impl SseCall {
  fn new(body: HttpBody, after: Option<Cursor>) -> Self {
    Self {
      body: Arc::new(body),
      state: Arc::new(Mutex::new(State {
        parser: Parser::default(),
        eof: false,
        done: false,
        last: after,
      })),
    }
  }

  fn state(&self) -> std::sync::MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The cursor of the last item read (or the one the call resumed after): send it as
  /// `after` to resume.
  pub fn last_cursor(&self) -> Option<Cursor> {
    self.state().last
  }

  fn step(&self) -> Step {
    let mut state = self.state();
    if state.done {
      return Step::End;
    }
    let event = match state.parser.next() {
      Ok(Some(event)) => event,
      Ok(None) if !state.eof => return Step::More,
      Ok(None) => {
        state.done = true;
        let ended = RpcClientError::Transport("the event stream ended without an exit".into());
        return Step::Event(CallEvent::Exit(Err(ended)));
      }
      Err(err) => {
        state.done = true;
        return Step::Event(CallEvent::Exit(Err(err)));
      }
    };
    let value = crate::codec::decode_value(event.data.as_bytes()).ok();
    if event.name == "exit" {
      state.done = true;
      return Step::Event(CallEvent::Exit(exit_result(value)));
    }
    let cursor = event.id.as_deref().and_then(|id| Cursor::decode(id).ok());
    if cursor.is_some() {
      state.last = cursor;
    }
    Step::Event(CallEvent::Item {
      value: value.unwrap_or(Value::Null),
      cursor,
    })
  }

  /// The next event: items, then one [`CallEvent::Exit`], then `None`.
  pub fn next(&self) -> Effect<Option<CallEvent>, RpcClientError> {
    let this = self.clone();
    suspend(move || match this.step() {
      Step::Event(event) => succeed(Some(event)),
      Step::End => succeed(None),
      Step::More => {
        let again = this.clone();
        this
          .body
          .next_chunk()
          .map_err(RpcClientError::Http)
          .flat_map(move |chunk| {
            {
              let mut state = again.state();
              match chunk {
                Some(bytes) => state.parser.feed(&bytes),
                None => state.eof = true,
              }
            }
            again.next()
          })
      }
    })
  }
}

// ---------------------------------------------------------------------------------------------
// Generated clients (`service::RpcTransport`)
// ---------------------------------------------------------------------------------------------

/// Generated clients (`<Trait>Client<SseTransport>`) over HTTP: queries and mutations through
/// [`HttpTransport`] (`POST /rpc`), subscriptions and server streams as server-sent events. Client
/// streams and two-way streams need the WebSocket transport ([`TransportError::Unsupported`]).
#[derive(Clone)]
pub struct SseTransport {
  client: RpcClient,
  http: HttpClientService,
  unary: HttpTransport,
}

impl std::fmt::Debug for SseTransport {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SseTransport")
      .field("url", &self.client.config().url())
      .finish_non_exhaustive()
  }
}

impl SseTransport {
  /// Call the server `client` names through `http`.
  pub fn new(client: RpcClient, http: HttpClientService) -> Self {
    Self {
      unary: HttpTransport::new(client.clone(), http.clone()),
      client,
      http,
    }
  }

  fn stream(
    &self,
    call: TransportCall,
    after: Option<Cursor>,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    let open = self
      .client
      .sse_version(&call.tag, Some(call.version), &call.payload, after)
      .provide_all(id_effect::hlist![self.http.clone()]);
    Box::pin(async move {
      let sse = match EffectExecutor::global().run_effect(open).await {
        Exit::Success(sse) => sse,
        Exit::Failure(cause) => return Err(transport_error(cause.fail_option())),
      };
      let frames = futures_util::stream::unfold(Some(sse), |sse| async move {
        let sse = sse?;
        let id = CallId::new(1).unwrap_or(CallId::CONNECTION);
        match EffectExecutor::global().run_effect(sse.next()).await {
          Exit::Success(Some(CallEvent::Item { value, cursor })) => {
            let mut item = Frame::new(id, FrameKind::Item).with_payload(value);
            item.cursor = cursor;
            Some((item, Some(sse)))
          }
          Exit::Success(Some(CallEvent::Exit(result))) => Some((exit_frame(id, result), None)),
          Exit::Success(None) => None,
          Exit::Failure(cause) => Some((exit_frame(id, Err(transport_error_of(&cause))), None)),
        }
      });
      Ok(Box::pin(frames) as FrameStream)
    })
  }
}

fn transport_error_of(cause: &id_effect::Cause<RpcClientError>) -> RpcClientError {
  RpcClientError::Transport(format!("{cause:?}"))
}

fn transport_error(error: Option<RpcClientError>) -> TransportError {
  match error {
    Some(RpcClientError::Refused(problem)) => TransportError::Refused {
      status: problem.status.as_u16(),
      problem: Some(problem),
    },
    Some(RpcClientError::UnexpectedStatus(status)) => TransportError::Refused {
      status,
      problem: None,
    },
    Some(other) => TransportError::Io(other.to_string()),
    None => TransportError::Io("the event stream could not be opened".to_owned()),
  }
}

/// The `exit` frame of a result: its value, its failure, or `Cancelled` when the stream broke.
fn exit_frame(id: CallId, result: Result<Value, RpcClientError>) -> Frame {
  let exit = match result {
    Ok(value) => WireExit::Success { value },
    Err(RpcClientError::Failure(error)) => WireExit::Failure { error },
    Err(_) => WireExit::Failure {
      error: serde_json::json!({"_tag": "Cancelled"}),
    },
  };
  Frame::new(id, FrameKind::Exit).with_payload(serde_json::to_value(exit).unwrap_or_default())
}

impl RpcTransport for SseTransport {
  fn unary(&self, call: TransportCall) -> BoxFuture<Result<WireExit, TransportError>> {
    self.unary.unary(call)
  }

  fn server_stream(&self, call: TransportCall) -> BoxFuture<Result<FrameStream, TransportError>> {
    self.stream(call, None)
  }

  fn subscribe(
    &self,
    call: TransportCall,
    after: Option<Cursor>,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    self.stream(call, after)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn drain(call: &SseCall) -> Vec<CallEvent> {
    let mut out = Vec::new();
    while let Ok(Some(event)) = call.next().run_blocking() {
      out.push(event);
    }
    out
  }

  trait Blocking<A> {
    fn run_blocking(self) -> Result<A, RpcClientError>;
  }

  impl<A: Send + 'static> Blocking<A> for Effect<A, RpcClientError> {
    fn run_blocking(self) -> Result<A, RpcClientError> {
      self
        .block_on()
        .into_result()
        .map_err(|cause| cause.fail_option().expect("a typed failure"))
    }
  }

  #[test]
  fn query_strings_are_percent_encoded() {
    assert_eq!(
      percent_encode(r#"{"a":"b c"}"#),
      "%7B%22a%22%3A%22b%20c%22%7D"
    );
    assert_eq!(percent_encode("Az09-_.~"), "Az09-_.~");
  }

  #[test]
  fn events_parse_across_chunks_and_line_ends() {
    let cursor = Cursor::events(9);
    let text = format!(
      ":\n\nretry: 10\n\nevent: item\r\ndata: {{\"a\":\r\ndata: 1}}\r\nid: {cursor}\r\n\r\nevent: item\ndata: 2\nid: nope\n\nevent: exit\ndata: {{\"_tag\":\"Success\",\"value\":3}}\n\nevent: item\ndata: 4\n\n"
    );
    let body = HttpBody::from_bytes(text.as_bytes(), 7);
    let call = SseCall::new(body, None);
    let events = drain(&call);
    assert_eq!(events.len(), 3);
    assert!(
      matches!(&events[0], CallEvent::Item { value, cursor: Some(c) } if value["a"] == 1 && *c == cursor)
    );
    assert!(matches!(&events[1], CallEvent::Item { value, cursor: None } if *value == 2));
    assert!(matches!(&events[2], CallEvent::Exit(Ok(v)) if *v == 3));
    assert_eq!(call.last_cursor(), Some(cursor));
    assert!(format!("{call:?}").contains("last_cursor"));
  }

  #[test]
  fn streams_without_an_exit_or_with_huge_events_fail() {
    let call = SseCall::new(HttpBody::from_chunks([b"data: 1\n\n".to_vec()]), None);
    let events = drain(&call);
    assert!(matches!(
      &events[..],
      [
        CallEvent::Item { .. },
        CallEvent::Exit(Err(RpcClientError::Transport(_)))
      ]
    ));
    let huge = vec![b'x'; MAX_EVENT_BYTES + 1];
    let call = SseCall::new(HttpBody::from_chunks([huge]), Some(Cursor::events(1)));
    assert_eq!(call.last_cursor(), Some(Cursor::events(1)));
    let events = drain(&call);
    assert!(
      matches!(&events[..], [CallEvent::Exit(Err(RpcClientError::Transport(m)))] if m.contains("over"))
    );
    let bad_exit = SseCall::new(
      HttpBody::from_chunks([b"event: exit\ndata: 1\n\n".to_vec()]),
      None,
    );
    assert!(matches!(
      &drain(&bad_exit)[..],
      [CallEvent::Exit(Err(RpcClientError::Transport(_)))]
    ));
  }
}
