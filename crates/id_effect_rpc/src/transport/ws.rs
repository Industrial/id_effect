//! Every procedure kind over one WebSocket (feature `ws`; design note §5): `GET {path}/ws`, frames
//! multiplexed by call id, on `id_effect_axum`'s realtime layer.
//!
//! ```rust
//! use std::sync::Arc;
//! use id_effect::{DynEnv, schema, succeed};
//! use id_effect_axum::realtime::ws::WsConfig;
//! use id_effect_rpc::transport::ws::RpcWsConfig;
//! use id_effect_rpc::{Procedure, RpcGroup, RpcServer};
//!
//! let mut group = RpcGroup::new();
//! group.register(
//!   Procedure::query("Greeter.greet").public(),
//!   Arc::new(schema::string::<()>()),
//!   Arc::new(schema::string::<()>()),
//!   &["message"],
//!   |name, _env| succeed(format!("Hello, {name}!")),
//! );
//! let socket = WsConfig::default().allowed_origins(["https://app.example.com"]).unwrap();
//! let router = RpcServer::new(group)
//!   .with_websocket(RpcWsConfig::default().socket(socket).max_calls(64))
//!   .router();
//! assert!(router.access_report().public.contains(&"GET /rpc/ws".to_owned()));
//! let app: axum::Router = router.with_state(DynEnv::new()).into_axum();
//! # let _ = app;
//! ```
//!
//! **Handshake.** The client offers subprotocols (`idrpc.json.v1`, and `idrpc.postcard.v1` when
//! that codec is built); the server picks the first it knows. An upgrade offering none is refused
//! with **415** Problem `UnsupportedSubprotocol` — there is no silent JSON fallback. The origin
//! allowlist, the connection caps (16 per client IP, 10,000 per endpoint: **429**), the 64 KiB
//! frame cap (close 1009), the message rate (close 1008), heartbeats and the idle timeout are the
//! realtime layer's ([`WsConfig`], passed in [`RpcWsConfig::socket`]). The route is public; each
//! call runs its procedure's own guards on the upgrade request's head.
//!
//! **Connection.** The server sends `hello` first (session, codec, `max_frame`, credit `window`).
//! Then, per call id: a `call` frame opens it (at most [`RpcWsConfig::max_calls`] open at once,
//! beyond which the call's `exit` is `ResourceExhausted { limit: "calls" }`); `item` / `end` carry
//! a client stream's or a two-way stream's input; `cancel` interrupts it (`Cancelled`); the server
//! answers `item`s, then one `exit`. A call refused before it ran (unknown tag, a guard) exits with
//! the [`Rejected`](super::REJECTED_TAG) failure. A subscription's `call` frame may carry the
//! `cursor` to resume after.
//!
//! **Backpressure (credits).** Each side may have at most the granted credits of `item`s of a call
//! outstanding: the window (default 32) at first, then whatever `credit` frames grant. The server
//! stops pulling a stream whose credits are spent, so its producer suspends and a slow client slows
//! only its own stream; it grants a client credits back as the handler consumes items (half a
//! window at a time). A client that sends past its credits fails that call with
//! `ResourceExhausted { limit: "credits" }`. Queries, mutations and client streams run under the
//! server's deadline cap; the other streams run until they end, are cancelled, or the socket closes
//! (every open call is then interrupted, finalizers run).
//!
//! **Protocol violations** close the socket (1000): an undecodable frame ([`super::frame::decode`]),
//! a `call` on an id already open, a client-sent `exit` or `hello`.
//!
//! **Sessions.** Every connection gets a [`WsSession`]: a 128-bit random [`SessionId`] bound to
//! the connection's principal ([`RpcServer::with_principal`]). Until two-way resume lands
//! (r12-e12-2-bidi-resume) nothing is buffered, so a `resume` answers every call it names with
//! `ResumeExpired`.

use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::Poll;

use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt as _;
use futures_util::future::{Either, select};
use futures_util::stream::FuturesUnordered;
use id_effect::{DynEnv, Effect, Exit, HNil, Never, from_async};
use id_effect_axum::realtime::ws::{self as realtime, WsConfig, WsMessage, WsSocket};
use id_effect_axum::route::{Endpoint, Problem, Public};
use id_effect_axum::{EffectExecutor, RequestCaps};
use serde_json::Value;
use tokio::sync::{Semaphore, mpsc};

use super::{copy_head, frame, rejection_outcome};
use crate::cancel::{CallControl, CancelHandle, cancel_pair};
use crate::codec::{CodecId, ProcedureCodecs, negotiate_subprotocol};
use crate::contract::ProcedureKind;
use crate::correlation::ensure_correlation_id;
use crate::handler::{BoxCall, CallContext, CallOutcome, InboundSender};
use crate::kinds::{CallFrames, exit_frame};
use crate::server::RpcServer;
use crate::server::dispatch::{self, Rejection};
use crate::wire::{
  CallId, Cursor, Frame, FrameFlags, FrameKind, Hello, INITIAL_CREDITS, MAX_FRAME_BYTES, Resume,
  RpcFailure, SessionId,
};

/// The WebSocket route, below the server's path: `GET {path}/ws`.
pub const PATH: &str = "/ws";

/// Default cap on the calls open at once on one connection (ADR 0014 C3).
pub const DEFAULT_MAX_CALLS: usize = 100;

/// The most credits a peer may hold for one call (further grants are clamped).
const MAX_GRANTED: usize = 1 << 20;

/// Encoded frames queued for the socket before the calls writing them wait.
const OUTBOX: usize = 64;

const TARGET: &str = "id_effect_rpc::transport::ws";

const SEC_WEBSOCKET_PROTOCOL: HeaderName = HeaderName::from_static("sec-websocket-protocol");

/// Settings of the WebSocket transport; [`Default`] is the secure default.
#[derive(Clone, Debug)]
pub struct RpcWsConfig {
  socket: WsConfig,
  max_calls: usize,
  window: u32,
}

impl Default for RpcWsConfig {
  fn default() -> Self {
    Self {
      socket: WsConfig::default(),
      max_calls: DEFAULT_MAX_CALLS,
      window: INITIAL_CREDITS,
    }
  }
}

impl RpcWsConfig {
  /// The secure defaults.
  pub fn new() -> Self {
    Self::default()
  }

  /// The socket's limits and origin allowlist (default [`WsConfig::default`]: every browser
  /// origin refused). Its subprotocols are replaced by the codecs this build knows.
  #[must_use]
  pub fn socket(mut self, socket: WsConfig) -> Self {
    self.socket = socket;
    self
  }

  /// At most `calls` (at least 1) calls open at once on one connection.
  #[must_use]
  pub fn max_calls(mut self, calls: usize) -> Self {
    self.max_calls = calls.max(1);
    self
  }

  /// The initial credit window of every call, in items (1 to 4,096).
  #[must_use]
  pub fn window(mut self, items: u32) -> Self {
    self.window = items.clamp(1, 4096);
    self
  }

  /// The socket settings.
  pub fn socket_config(&self) -> &WsConfig {
    &self.socket
  }

  /// The per-connection call cap.
  pub fn calls_limit(&self) -> usize {
    self.max_calls
  }

  /// The initial credit window.
  pub fn credit_window(&self) -> u32 {
    self.window
  }
}

/// The resume session of one connection: a 128-bit random id bound to the principal that opened
/// it (C8). r12-e12-2-bidi-resume adds its replay buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsSession {
  id: SessionId,
  principal: Option<String>,
}

impl WsSession {
  /// A fresh session for `principal` (`None`: anonymous).
  pub fn new(principal: Option<String>) -> Self {
    Self {
      id: SessionId::random(),
      principal,
    }
  }

  /// The session id the `hello` frame carries.
  pub fn id(&self) -> SessionId {
    self.id
  }

  /// The principal the session is bound to.
  pub fn principal(&self) -> Option<&str> {
    self.principal.as_deref()
  }
}

/// The upgrade request's head, for the guards of the calls on its socket.
struct UpgradeHead(Parts);

struct Shared {
  server: RpcServer,
  config: RpcWsConfig,
}

/// The subprotocols this build serves, in preference order.
fn subprotocols() -> impl Iterator<Item = &'static str> {
  CodecId::ALL.iter().map(|codec| codec.subprotocol())
}

fn offered_codec(headers: &HeaderMap) -> Option<CodecId> {
  negotiate_subprotocol(
    headers
      .get_all(SEC_WEBSOCKET_PROTOCOL)
      .iter()
      .filter_map(|v| v.to_str().ok())
      .flat_map(|v| v.split(',')),
  )
}

fn is_upgrade(headers: &HeaderMap) -> bool {
  headers
    .get_all(header::UPGRADE)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .any(|v| v.trim().eq_ignore_ascii_case("websocket"))
}

/// Refuses an upgrade that offers no codec, and records the request head for the calls' guards.
async fn admit(request: Request, next: Next) -> Response {
  let (mut parts, body) = request.into_parts();
  if is_upgrade(&parts.headers) && offered_codec(&parts.headers).is_none() {
    let offer = subprotocols().collect::<Vec<_>>().join(", ");
    return Problem::new(StatusCode::UNSUPPORTED_MEDIA_TYPE)
      .with_title("Unsupported subprotocol")
      .with_detail(format!("offer one of the rpc subprotocols: {offer}"))
      .with_extension("code", Value::String("UnsupportedSubprotocol".into()))
      .into_response();
  }
  let head = copy_head(&parts);
  let mut caps = parts
    .extensions
    .get::<RequestCaps>()
    .cloned()
    .unwrap_or_default();
  caps.insert(UpgradeHead(head));
  parts.extensions.insert(caps);
  next.run(Request::from_parts(parts, body)).await
}

/// The typed route `GET {path}/ws` serving `server`'s procedures over WebSocket (what
/// [`RpcServer::with_websocket`] mounts).
pub fn endpoint(server: RpcServer, config: RpcWsConfig) -> Endpoint<DynEnv, Public> {
  let path = format!("{}{PATH}", server.path().trim_end_matches('/'));
  let socket = config.socket.clone().protocols(subprotocols());
  let shared = Arc::new(Shared { server, config });
  let handler = move |env: &mut DynEnv, caps: RequestCaps, socket: WsSocket| {
    let head = caps
      .get::<UpgradeHead>()
      .map_or_else(blank_head, |head| copy_head(&head.0));
    // `admit` refused upgrades offering no codec, so the realtime layer selected one of ours.
    let codec = socket
      .protocol()
      .and_then(|p| negotiate_subprotocol([p]))
      .unwrap_or(CodecId::Json);
    serve(Arc::clone(&shared), caps.overlay(env), head, codec, socket)
  };
  realtime::endpoint(path, socket, handler)
    .layer(axum::middleware::from_fn(admit))
    .public()
}

fn blank_head() -> Parts {
  axum::http::Request::new(()).into_parts().0
}

/// Serve one already upgraded socket (for [`realtime::serve_io`]: an in-memory pipe, an HTTP/2
/// extended CONNECT stream): `head` is the request head the procedures' guards see, `env` the
/// environment the calls run with, `codec` the one the handshake negotiated.
pub fn serve_socket<R>(
  server: &RpcServer,
  config: &RpcWsConfig,
  env: DynEnv,
  head: Parts,
  codec: CodecId,
  socket: WsSocket,
) -> Effect<(), Never, R>
where
  R: Send + Sync + 'static,
{
  let shared = Arc::new(Shared {
    server: server.clone(),
    config: config.clone(),
  });
  serve(shared, env, head, codec, socket)
}

fn serve<R>(
  shared: Arc<Shared>,
  env: DynEnv,
  head: Parts,
  codec: CodecId,
  socket: WsSocket,
) -> Effect<(), Never, R>
where
  R: Send + Sync + 'static,
{
  from_async(move || async move {
    connection(shared, env, head, codec, socket).await;
    Ok(())
  })
}

async fn connection(
  shared: Arc<Shared>,
  env: DynEnv,
  head: Parts,
  codec: CodecId,
  socket: WsSocket,
) {
  // Every call on this socket runs with the same connection id.
  let mut env = env;
  env.insert_arc(Arc::new(super::ConnectionId::fresh()));
  let (incoming, sender) = socket.into_parts::<Never, HNil>();
  let (in_tx, in_rx) = mpsc::channel::<WsMessage>(1);
  let reader = EffectExecutor::global().run_effect(incoming.run_for_each_effect(move |msg| {
    let tx = in_tx.clone();
    from_async(move || async move {
      let _ = tx.send(msg).await;
      Ok(())
    })
  }));
  let (out_tx, mut out_rx) = mpsc::channel::<WsMessage>(OUTBOX);
  let writer = async move {
    while let Some(msg) = out_rx.recv().await {
      let sent = EffectExecutor::global()
        .run_effect(sender.send::<HNil>(msg))
        .await;
      if !matches!(sent, Exit::Success(())) {
        break;
      }
    }
  };
  let session = WsSession::new(shared.server.principal_of(&head));
  let conn = Conn {
    cid: ensure_correlation_id(&head.headers),
    head: Arc::new(head),
    env,
    out: Outbox { tx: out_tx, codec },
    session,
    calls: HashMap::new(),
    running: FuturesUnordered::new(),
    shared,
  };
  let work = futures_util::future::join(conn.run(in_rx), writer);
  if let Either::Right((_, work)) = select(Box::pin(work), Box::pin(reader)).await {
    // The socket's inbound side ended: the calls see it, finish, and the writer drains.
    work.await;
  }
}

/// Why a frame could not be queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendError {
  /// Over the frame cap.
  TooLarge,
  /// The codec cannot carry the payload (a binary codec without the procedure's typed bridge).
  #[cfg_attr(not(feature = "postcard"), allow(dead_code))]
  Encode,
  /// The socket is gone.
  Closed,
}

/// The connection's outbound queue, encoding frames with its codec.
#[derive(Clone)]
struct Outbox {
  tx: mpsc::Sender<WsMessage>,
  codec: CodecId,
}

impl Outbox {
  /// Queue a connection frame, or a call's frame whose payload needs no typed bridge.
  async fn send(&self, frame: &Frame) -> Result<(), SendError> {
    self.send_with(frame, &ProcedureCodecs::default()).await
  }

  /// Queue a call's frame: JSON frames are text messages; postcard frames are binary, their
  /// payload through the procedure's typed bridge (`codecs`).
  async fn send_with(&self, frame: &Frame, codecs: &ProcedureCodecs) -> Result<(), SendError> {
    let msg = match self.codec {
      CodecId::Json => {
        let bytes = frame::encode(frame, CodecId::Json).map_err(|_| SendError::TooLarge)?;
        WsMessage::Text(String::from_utf8_lossy(&bytes).into_owned())
      }
      #[cfg(feature = "postcard")]
      CodecId::Postcard => crate::codec::postcard::encode_server_frame(codecs, frame)
        .map(WsMessage::from)
        .map_err(|err| match err {
          crate::codec::CodecError::TooLarge { .. } => SendError::TooLarge,
          _ => SendError::Encode,
        })?,
    };
    #[cfg(not(feature = "postcard"))]
    let _ = codecs;
    self.tx.send(msg).await.map_err(|_| SendError::Closed)
  }
}

/// One open call, as the connection loop routes frames to it.
struct Open {
  /// Where client items go (client streams and two-way streams, until the input ends).
  inbound: Option<mpsc::Sender<Value>>,
  /// Items the client may still send.
  in_credits: Arc<AtomicU32>,
  /// Items the server may still send.
  out_credits: Arc<Semaphore>,
  cancel: CancelHandle,
  /// A failure that replaces the call's own exit (credit overrun).
  failure: Arc<OnceLock<RpcFailure>>,
  /// The procedure's typed bridges (a binary codec decodes its items through them).
  #[cfg_attr(not(feature = "postcard"), allow(dead_code))]
  codecs: ProcedureCodecs,
}

impl Open {
  fn stop(&mut self, failure: Option<RpcFailure>) {
    if let Some(failure) = failure {
      let _ = self.failure.set(failure);
    }
    self.inbound = None;
    self.cancel.cancel();
    self.out_credits.close();
  }
}

enum Event {
  Message(Option<WsMessage>),
  Done(CallId),
}

type Running = FuturesUnordered<Pin<Box<dyn Future<Output = CallId> + Send>>>;

/// The connection loop: decodes frames, opens and routes calls.
struct Conn {
  shared: Arc<Shared>,
  env: DynEnv,
  head: Arc<Parts>,
  cid: String,
  out: Outbox,
  session: WsSession,
  calls: HashMap<CallId, Open>,
  running: Running,
}

impl Conn {
  async fn run(mut self, mut inbox: mpsc::Receiver<WsMessage>) {
    // A socket gone before the hello ends the loop below: its inbox is closed.
    let _ = self.hello().await;
    loop {
      let running = &mut self.running;
      let event = poll_fn(|cx| {
        if let Poll::Ready(Some(id)) = running.poll_next_unpin(cx) {
          return Poll::Ready(Event::Done(id));
        }
        inbox.poll_recv(cx).map(Event::Message)
      })
      .await;
      match event {
        Event::Done(id) => {
          self.calls.remove(&id);
        }
        Event::Message(None) => return,
        Event::Message(Some(msg)) => {
          if let Err(why) = self.on_message(&msg).await {
            tracing::debug!(target: TARGET, %why, "rpc websocket closed on a protocol violation");
            return;
          }
        }
      }
    }
  }

  // `&mut`: the running calls are `Send` but not `Sync`.
  async fn hello(&mut self) -> Result<(), SendError> {
    let max_frame = self.shared.config.socket.max_message().min(MAX_FRAME_BYTES);
    let hello = Hello {
      session: self.session.id(),
      codec: self.out.codec.subprotocol().to_owned(),
      max_frame: u32::try_from(max_frame).unwrap_or(u32::MAX),
      window: self.shared.config.window,
    };
    let payload = serde_json::to_value(&hello).unwrap_or_default();
    let frame = Frame::new(CallId::CONNECTION, FrameKind::Hello).with_payload(payload);
    self.out.send(&frame).await
  }

  /// Decode one inbound message; `Ok(None)` when its payload was refused (and answered).
  async fn decode(&mut self, msg: &WsMessage) -> Result<Option<Frame>, String> {
    match self.out.codec {
      CodecId::Json => frame::decode(msg.as_bytes(), CodecId::Json)
        .map(Some)
        .map_err(|err| err.to_string()),
      #[cfg(feature = "postcard")]
      CodecId::Postcard => self.decode_postcard(msg.as_bytes()).await,
    }
  }

  /// A postcard frame: the envelope, then its payload through the procedure's typed bridge.
  #[cfg(feature = "postcard")]
  async fn decode_postcard(&mut self, bytes: &[u8]) -> Result<Option<Frame>, String> {
    use crate::codec::Codec as _;
    use crate::codec::postcard::{Postcard, decode_frame};
    let raw = decode_frame(bytes).map_err(|err| err.to_string())?;
    frame::validate(&raw).map_err(|err| err.to_string())?;
    let Frame {
      id,
      kind,
      flags,
      tag,
      v,
      cursor,
      credits,
      payload,
    } = raw;
    let group = self.shared.server.group();
    let decoded = match (kind, payload) {
      (FrameKind::Resume, Some(bytes)) => Some(
        Postcard::decode::<Resume>(&bytes)
          .map(|resume| serde_json::to_value(resume).unwrap_or_default()),
      ),
      (FrameKind::Call, Some(bytes)) => {
        let entry = tag
          .as_deref()
          .and_then(|tag| dispatch::resolve(group, tag, v).ok());
        // An unknown procedure is refused when the call runs (`Rejected`, 404).
        entry.map(|entry| entry.codecs().decode_input(CodecId::Postcard, &bytes))
      }
      (FrameKind::Item, Some(bytes)) => self
        .calls
        .get(&id)
        .map(|open| open.codecs.decode_input(CodecId::Postcard, &bytes)),
      _ => None,
    };
    let payload = match decoded.transpose() {
      Ok(payload) => payload,
      Err(err) if kind == FrameKind::Resume => return Err(format!("malformed resume: {err}")),
      Err(err) => {
        self.refuse_payload(id, kind, &err).await;
        return Ok(None);
      }
    };
    Ok(Some(Frame {
      id,
      kind,
      flags,
      tag,
      v,
      cursor,
      credits,
      payload,
    }))
  }

  /// A `call` or `item` whose payload the procedure's bridge refused: no bridge is `Rejected`
  /// 415, bytes that are not the type `InvalidInput`.
  #[cfg(feature = "postcard")]
  async fn refuse_payload(&mut self, id: CallId, kind: FrameKind, err: &crate::codec::CodecError) {
    let issues = vec![crate::wire::InputIssue {
      path: String::new(),
      message: err.to_string(),
    }];
    if kind == FrameKind::Item {
      if let Some(open) = self.calls.get_mut(&id) {
        open.stop(Some(RpcFailure::InvalidInput { issues }));
      }
      return;
    }
    let outcome = match err {
      crate::codec::CodecError::Unsupported(_) => {
        let status = axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE;
        let error = serde_json::json!({
          "_tag": super::REJECTED_TAG,
          "status": status.as_u16(),
          "title": "Unsupported Media Type",
        });
        CallOutcome::typed(error, status)
      }
      _ => CallOutcome::invalid_input(issues),
    };
    if !self.calls.contains_key(&id) {
      let _ = self.out.send(&exit_frame(id, &outcome)).await;
    }
  }

  async fn on_message(&mut self, msg: &WsMessage) -> Result<(), String> {
    let Some(frame) = self.decode(msg).await? else {
      return Ok(());
    };
    let id = frame.id;
    match frame.kind {
      FrameKind::Call => return self.open_call(frame).await,
      FrameKind::Item => self.item(frame),
      FrameKind::End => {
        if let Some(open) = self.calls.get_mut(&id) {
          open.inbound = None;
        }
      }
      FrameKind::Cancel => {
        if let Some(open) = self.calls.get_mut(&id) {
          open.stop(None);
        }
      }
      FrameKind::Credit => {
        if let Some(open) = self.calls.get(&id) {
          let granted = frame.credits.unwrap_or(0) as usize;
          let room = MAX_GRANTED.saturating_sub(open.out_credits.available_permits());
          open.out_credits.add_permits(granted.min(room));
        }
      }
      FrameKind::Ping => {
        let _ = self
          .out
          .send(&Frame::new(CallId::CONNECTION, FrameKind::Pong))
          .await;
      }
      FrameKind::Pong => {}
      FrameKind::Resume => return self.resume(frame).await,
      kind @ (FrameKind::Exit | FrameKind::Hello) => {
        return Err(format!("a client may not send `{kind:?}` frames"));
      }
    }
    Ok(())
  }

  fn item(&mut self, frame: Frame) {
    let Some(open) = self.calls.get_mut(&frame.id) else {
      // An item for a call that already ended: the client has not seen the exit yet.
      return;
    };
    let end = frame.flags.contains(FrameFlags::END);
    if let Some(tx) = &open.inbound {
      if open.in_credits.load(Ordering::Acquire) == 0 {
        let limit = "credits".to_owned();
        return open.stop(Some(RpcFailure::ResourceExhausted { limit }));
      }
      // The queue holds at most the credits granted, so it is never full here; a send fails only
      // when the handler stopped reading its input, and the item has nowhere to go.
      if tx.try_send(frame.payload.unwrap_or(Value::Null)).is_ok() {
        open.in_credits.fetch_sub(1, Ordering::AcqRel);
      }
    }
    if end {
      open.inbound = None;
    }
  }

  async fn open_call(&mut self, frame: Frame) -> Result<(), String> {
    let id = frame.id;
    if self.calls.contains_key(&id) {
      return Err(format!("call id {} is already open", id.get()));
    }
    if self.calls.len() >= self.shared.config.max_calls {
      let limit = "calls".to_owned();
      let outcome = CallOutcome::failure(RpcFailure::ResourceExhausted { limit });
      let _ = self.out.send(&exit_frame(id, &outcome)).await;
      return Ok(());
    }
    let server = &self.shared.server;
    let tag = frame.tag.unwrap_or_default();
    let entry = dispatch::resolve(server.group(), &tag, frame.v).ok();
    let kind = entry.map(|entry| entry.kind());
    let codecs = entry
      .map(|entry| entry.codecs().clone())
      .unwrap_or_default();
    let window = self.shared.config.window;
    let (inbound, inbound_rx) = match kind {
      Some(ProcedureKind::ClientStream | ProcedureKind::Bidi) => {
        let (tx, rx) = mpsc::channel(window as usize);
        (Some(tx), Some(rx))
      }
      _ => (None, None),
    };
    let (cancel, signal) = cancel_pair();
    let mut control = CallControl::none().with_cancel(signal);
    if matches!(
      kind,
      Some(ProcedureKind::Query | ProcedureKind::Mutation | ProcedureKind::ClientStream)
    ) {
      control = control.with_deadline(server.max_deadline());
    }
    let open = Open {
      inbound,
      in_credits: Arc::new(AtomicU32::new(window)),
      out_credits: Arc::new(Semaphore::new(window as usize)),
      cancel,
      failure: Arc::new(OnceLock::new()),
      codecs: codecs.clone(),
    };
    let task = CallTask {
      server: server.clone(),
      head: Arc::clone(&self.head),
      ctx: CallContext::new(self.env.clone(), format!("{}-{}", self.cid, id.get())),
      control,
      out: self.out.clone(),
      id,
      tag,
      v: frame.v,
      payload: frame.payload.unwrap_or(Value::Null),
      cursor: frame.cursor,
      window: server.stream_window(),
      credit_window: window,
      in_credits: Arc::clone(&open.in_credits),
      out_credits: Arc::clone(&open.out_credits),
      inbound: inbound_rx,
      failure: Arc::clone(&open.failure),
      codecs,
    };
    self.calls.insert(id, open);
    self.running.push(Box::pin(task.run()));
    Ok(())
  }

  /// r12-e12-2-bidi-resume replaces this: replay each call from the session's buffer when
  /// `resume.session` is a live session of this connection's principal. Nothing is buffered yet,
  /// so every call named is expired.
  async fn resume(&mut self, frame: Frame) -> Result<(), String> {
    let resume: Resume = serde_json::from_value(frame.payload.unwrap_or_default())
      .map_err(|err| format!("malformed resume: {err}"))?;
    let expired = CallOutcome::failure(RpcFailure::ResumeExpired);
    for call in resume.calls {
      if call.id != CallId::CONNECTION && !self.calls.contains_key(&call.id) {
        let _ = self.out.send(&exit_frame(call.id, &expired)).await;
      }
    }
    Ok(())
  }
}

/// A started call, whatever its kind.
enum Started {
  Unary(BoxCall),
  Frames(CallFrames),
  Client(InboundSender, BoxCall),
  Bidi(InboundSender, CallFrames),
}

/// One call: lookup and guards, then the kind's frames.
struct CallTask {
  server: RpcServer,
  head: Arc<Parts>,
  ctx: CallContext,
  control: CallControl,
  out: Outbox,
  id: CallId,
  tag: String,
  v: Option<u32>,
  payload: Value,
  cursor: Option<Cursor>,
  window: usize,
  credit_window: u32,
  in_credits: Arc<AtomicU32>,
  out_credits: Arc<Semaphore>,
  inbound: Option<mpsc::Receiver<Value>>,
  failure: Arc<OnceLock<RpcFailure>>,
  codecs: ProcedureCodecs,
}

impl CallTask {
  async fn run(self) -> CallId {
    let id = self.id;
    self.execute().await;
    id
  }

  async fn start(&mut self) -> Result<Started, Rejection> {
    let entry = dispatch::resolve(self.server.group(), &self.tag, self.v)?;
    dispatch::authorize(entry, &self.head).await?;
    let (id, window, ctx, control) = (self.id, self.window, self.ctx.clone(), self.control.clone());
    let payload = std::mem::take(&mut self.payload);
    match entry.kind() {
      ProcedureKind::Query | ProcedureKind::Mutation => {
        dispatch::start_unary(entry, payload, ctx, control).map(Started::Unary)
      }
      ProcedureKind::ServerStream => {
        dispatch::start_server_stream(entry, id, payload, ctx, control, window).map(Started::Frames)
      }
      ProcedureKind::Subscription => {
        let after = self.cursor;
        dispatch::start_subscription(entry, id, payload, after, ctx, control, window)
          .map(Started::Frames)
      }
      ProcedureKind::ClientStream => dispatch::start_client_stream(entry, ctx, control, 1)
        .map(|(tx, run)| Started::Client(tx, run)),
      ProcedureKind::Bidi => dispatch::start_bidi(entry, id, ctx, control, window)
        .map(|(tx, frames)| Started::Bidi(tx, frames)),
    }
  }

  async fn execute(mut self) {
    let started = match self.start().await {
      Ok(started) => started,
      Err(rejection) => return self.finish(rejection_outcome(&rejection)).await,
    };
    let inbound = self.inbound.take();
    match started {
      Started::Unary(run) => self.finish(run.await).await,
      Started::Frames(frames) => self.forward(frames).await,
      Started::Client(tx, run) => {
        let feed = self.feed(inbound, tx);
        let outcome = match select(run, Box::pin(feed)).await {
          Either::Left((outcome, _feed)) => outcome,
          Either::Right(((), run)) => run.await,
        };
        self.finish(outcome).await;
      }
      Started::Bidi(tx, frames) => {
        let feed = self.feed(inbound, tx);
        let forward = self.forward(frames);
        if let Either::Right(((), forward)) = select(Box::pin(forward), Box::pin(feed)).await {
          forward.await;
        }
      }
    }
  }

  /// Send the call's `exit` (the recorded failure instead, if one was).
  async fn finish(&self, outcome: CallOutcome) {
    self.send_exit(exit_frame(self.id, &outcome)).await;
  }

  /// Send `exit` (the recorded failure instead, if one was); one the codec cannot carry becomes
  /// `InternalError`.
  async fn send_exit(&self, exit: Frame) {
    let exit = match self.failure.get() {
      Some(failure) => exit_frame(self.id, &CallOutcome::failure(failure.clone())),
      None => exit,
    };
    if let Err(SendError::TooLarge | SendError::Encode) =
      self.out.send_with(&exit, &self.codecs).await
    {
      let internal = CallOutcome::internal(&self.ctx.correlation_id);
      let _ = self.out.send(&exit_frame(self.id, &internal)).await;
    }
  }

  /// Send a stream's frames, an `item` only against a credit.
  async fn forward(&self, mut frames: CallFrames) {
    while let Some(frame) = frames.next().await {
      if frame.kind == FrameKind::Exit {
        return self.send_exit(frame).await;
      }
      let Ok(permit) = self.out_credits.acquire().await else {
        // Cancelled (or overrun) while waiting for a credit: dropping the frames interrupts it.
        let _ = self.failure.set(RpcFailure::Cancelled);
        drop(frames);
        return self.finish(CallOutcome::success(Value::Null)).await;
      };
      permit.forget();
      match self.out.send_with(&frame, &self.codecs).await {
        Ok(()) => {}
        Err(SendError::Closed) => return,
        Err(err) => {
          let failure = match err {
            SendError::TooLarge => RpcFailure::ResourceExhausted {
              limit: "max_frame".to_owned(),
            },
            _ => RpcFailure::InternalError {
              id: self.ctx.correlation_id.to_string(),
            },
          };
          let _ = self.failure.set(failure);
          drop(frames);
          return self.finish(CallOutcome::success(Value::Null)).await;
        }
      }
    }
  }

  /// Hand the client's items to the call, granting credits back half a window at a time; ends
  /// the input when the client does.
  async fn feed(&self, inbound: Option<mpsc::Receiver<Value>>, tx: InboundSender) {
    // Client streams and two-way streams always have their queue.
    let mut rx = inbound.unwrap_or_else(|| mpsc::channel(1).1);
    let batch = (self.credit_window / 2).max(1);
    let mut taken = 0;
    while let Some(item) = rx.recv().await {
      if !tx.send(item).await {
        return;
      }
      taken += 1;
      if taken >= batch {
        self.in_credits.fetch_add(taken, Ordering::AcqRel);
        if self.out.send(&Frame::credit(self.id, taken)).await.is_err() {
          return;
        }
        taken = 0;
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn config_clamps_and_reports() {
    let config = RpcWsConfig::new().max_calls(0).window(0);
    assert_eq!((config.calls_limit(), config.credit_window()), (1, 1));
    assert_eq!(RpcWsConfig::new().window(1 << 20).credit_window(), 4096);
    let socket = WsConfig::default().messages_per_second(7);
    let config = RpcWsConfig::default().socket(socket);
    assert_eq!(config.socket_config().message_rate(), 7);
    assert_eq!(config.calls_limit(), DEFAULT_MAX_CALLS);
  }

  #[test]
  fn sessions_are_random_and_bound() {
    let a = WsSession::new(Some("ada".into()));
    let b = WsSession::new(None);
    assert_ne!(a.id(), b.id());
    assert_eq!((a.principal(), b.principal()), (Some("ada"), None));
  }

  #[test]
  fn subprotocols_are_negotiated_from_headers() {
    let mut headers = HeaderMap::new();
    assert_eq!(offered_codec(&headers), None);
    assert!(!is_upgrade(&headers));
    headers.append(header::UPGRADE, "WebSocket".parse().unwrap());
    headers.append(
      SEC_WEBSOCKET_PROTOCOL,
      "chat, idrpc.json.v1".parse().unwrap(),
    );
    assert!(is_upgrade(&headers));
    assert_eq!(offered_codec(&headers), Some(CodecId::Json));
    let offered = subprotocols().collect::<Vec<_>>();
    assert_eq!(offered[0], "idrpc.json.v1");
    assert_eq!(offered.len(), CodecId::ALL.len());
  }
}
