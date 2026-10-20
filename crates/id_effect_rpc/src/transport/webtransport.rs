//! Every procedure kind over a WebTransport session (feature `webtransport`; design note §5):
//! `CONNECT {path}/wt` on HTTP/3, **one QUIC stream per call**, on `id_effect_axum`'s
//! WebTransport sessions ([`id_effect_axum::server::webtransport`]).
//!
//! The connection logic is the WebSocket transport's ([`super::ws`]): the same frames, `hello`,
//! credits, call cap, guards, sessions and resume ([`RpcWsConfig`]). What differs is how frames
//! travel:
//!
//! - **Records.** Each frame is one record on a stream: its length as a u32 big-endian, then the
//!   codec bytes ([`encode_record`] / [`split_record`]). A length over 64 KiB is refused before
//!   anything is buffered, and closes the session.
//! - **The control stream.** The client opens it first: a bidirectional stream whose first
//!   record is a connection frame (id 0: `ping`, or `resume`). The server's `hello`, `pong`,
//!   `ping` and every frame of a call that has no stream of its own (a resumed call's replay)
//!   travel on it. One per session; a second closes the session.
//! - **Call streams.** Every other stream belongs to the call id of its first record: a `call`
//!   frame opens the call there, and its `item`s, `credit`s and `exit` travel on that stream (no
//!   head-of-line blocking between calls); the server finishes the stream after the `exit`. A
//!   client may move a resumed call onto a new stream by sending its next frame (a `credit`)
//!   there. A record for another id on a call stream closes the session.
//! - **Codec.** `?codec=idrpc.postcard.v1` on the CONNECT picks postcard; JSON otherwise. An
//!   unknown codec is refused with **415** Problem `UnsupportedSubprotocol`.
//! - **Heartbeat.** WebTransport has no ping of its own, so the server sends a `ping` frame on
//!   the control stream every 20 s and closes the session after two intervals without a record
//!   from the client (any record counts; answer `ping` with `pong`). The control stream must be
//!   open by then too.
//!
//! The CONNECT is a typed route, public like the WebSocket one: each call runs its procedure's
//! guards on the CONNECT's head. Its `Origin` is checked against the session config's allowlist
//! (empty by default: every browser origin refused, ADR 0013 R9 / ADR 0014 C4), and sessions are
//! capped per endpoint and per client IP ([`WebTransportConfig`]).
//!
//! **Resume.** A session that is lost (the browser tab sleeps, the network changes) leaves its
//! resumable calls detached exactly as a dropped WebSocket does. The client opens a new session,
//! opens its control stream with `resume {session, calls: [{id, cursor}]}`, and the replay
//! arrives there. Give the WebSocket transport a clone of the same [`RpcWsConfig`] and a client
//! can resume a WebTransport session over WebSocket, or the other way around (clones share one
//! session store).
//!
//! ```rust
//! use std::sync::Arc;
//! use id_effect::{DynEnv, schema, succeed};
//! use id_effect_axum::server::webtransport::WebTransportConfig;
//! use id_effect_rpc::transport::webtransport::{self, RpcWebTransportConfig};
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
//! let session = WebTransportConfig::default()
//!   .allowed_origins(["https://app.example.com"])
//!   .unwrap();
//! let rpc = RpcWsConfig::default().max_calls(64);
//! let config = RpcWebTransportConfig::default().session(session).rpc(rpc);
//! let route = webtransport::endpoint(RpcServer::new(group), config);
//! let app = id_effect_axum::route::Router::new().route(route);
//! assert!(app.access_report().public.contains(&"CONNECT /rpc/wt".to_owned()));
//! ```

use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use id_effect::{DynEnv, Effect, Never, from_async};
use id_effect_axum::realtime::ws::{WsMessage, WsSocket};
use id_effect_axum::route::{Endpoint, Problem, Public};
use id_effect_axum::server::webtransport::{
  self as wt, RecvHalf, SendHalf, WebTransportConfig, WebTransportSession, WebTransportStream,
};
use id_effect_axum::{EffectExecutor, RequestCaps};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::copy_head;
use super::ws::{self, RpcWsConfig};
use crate::codec::postcard::{encode_record, split_record};
use crate::codec::{CodecId, ProcedureCodecs, negotiate_subprotocol};
use crate::server::RpcServer;
use crate::wire::{CallId, Frame, FrameKind};

/// The WebTransport route, below the server's path: `CONNECT {path}/wt`.
pub const PATH: &str = "/wt";

/// The CONNECT query parameter naming the codec (`?codec=idrpc.postcard.v1`).
pub const CODEC_PARAM: &str = "codec";

/// Default heartbeat: a `ping` every 20 s, two intervals of silence close the session.
pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(20);

/// Records queued for one stream (a call's credit window bounds what a call sends).
const STREAM_QUEUE: usize = 64;

/// Records queued for the control stream before it is open (the `hello` and a few more).
const CONTROL_QUEUE: usize = 64;

/// Frames of the session queued for the connection logic.
const INBOX: usize = 16;

/// Frames the connection logic queues for the streams.
const OUTBOX: usize = 64;

/// `rpc.transport` of this transport's spans.
const TRANSPORT: &str = "webtransport";

/// `H3_NO_ERROR`.
const CLOSE_OK: u32 = 0x100;

/// `H3_GENERAL_PROTOCOL_ERROR`: a record broke the rules above.
const CLOSE_PROTOCOL: u32 = 0x101;

const TARGET: &str = "id_effect_rpc::transport::webtransport";

/// Settings of the WebTransport transport; [`Default`] is the secure default.
#[derive(Clone, Debug)]
pub struct RpcWebTransportConfig {
  session: WebTransportConfig,
  rpc: RpcWsConfig,
  heartbeat: Duration,
  missed: u32,
}

impl Default for RpcWebTransportConfig {
  fn default() -> Self {
    Self {
      session: WebTransportConfig::default(),
      rpc: RpcWsConfig::default(),
      heartbeat: DEFAULT_HEARTBEAT,
      missed: 2,
    }
  }
}

impl RpcWebTransportConfig {
  /// The secure defaults.
  pub fn new() -> Self {
    Self::default()
  }

  /// The session settings: origin allowlist and session caps (default
  /// [`WebTransportConfig::default`]: every browser origin refused).
  #[must_use]
  pub fn session(mut self, session: WebTransportConfig) -> Self {
    self.session = session;
    self
  }

  /// The connection settings shared with the WebSocket transport: call cap, credit window,
  /// resume store ([`RpcWsConfig`]; its socket settings do not apply here).
  #[must_use]
  pub fn rpc(mut self, rpc: RpcWsConfig) -> Self {
    self.rpc = rpc;
    self
  }

  /// A `ping` every `interval` (at least 1 ms); `missed` intervals (at least 1) without a
  /// record from the client close the session.
  #[must_use]
  pub fn heartbeat(mut self, interval: Duration, missed: u32) -> Self {
    self.heartbeat = interval.max(Duration::from_millis(1));
    self.missed = missed.max(1);
    self
  }

  /// The session settings.
  pub fn session_config(&self) -> &WebTransportConfig {
    &self.session
  }

  /// The connection settings.
  pub fn rpc_config(&self) -> &RpcWsConfig {
    &self.rpc
  }

  /// The heartbeat interval and missed intervals.
  pub fn heartbeat_settings(&self) -> (Duration, u32) {
    (self.heartbeat, self.missed)
  }
}

/// The CONNECT's head and codec, for the calls of its session.
#[derive(Clone)]
struct ConnectHead {
  head: Arc<Parts>,
  codec: CodecId,
}

struct Shared {
  server: RpcServer,
  config: RpcWebTransportConfig,
}

/// The codec the CONNECT's query names (`None`: JSON), or the name it gave that is unknown.
fn query_codec(query: Option<&str>) -> Result<CodecId, String> {
  let named = query
    .unwrap_or_default()
    .split('&')
    .filter_map(|pair| pair.split_once('='))
    .find(|(key, _)| *key == CODEC_PARAM)
    .map(|(_, value)| value);
  match named {
    None => Ok(CodecId::Json),
    Some(name) => negotiate_subprotocol([name]).ok_or_else(|| name.to_owned()),
  }
}

/// Refuses a CONNECT naming an unknown codec, and records the head for the calls' guards.
async fn admit(request: Request, next: Next) -> Response {
  let (mut parts, body) = request.into_parts();
  let codec = match query_codec(parts.uri.query()) {
    Ok(codec) => codec,
    Err(name) => {
      let offer = CodecId::ALL
        .iter()
        .map(|codec| codec.subprotocol())
        .collect::<Vec<_>>()
        .join(", ");
      return Problem::new(StatusCode::UNSUPPORTED_MEDIA_TYPE)
        .with_title("Unsupported subprotocol")
        .with_detail(format!("unknown codec {name:?}: name one of {offer}"))
        .with_extension("code", Value::String("UnsupportedSubprotocol".into()))
        .into_response();
    }
  };
  let head = ConnectHead {
    head: Arc::new(copy_head(&parts)),
    codec,
  };
  let mut caps = parts
    .extensions
    .get::<RequestCaps>()
    .cloned()
    .unwrap_or_default();
  caps.insert(head);
  parts.extensions.insert(caps);
  next.run(Request::from_parts(parts, body)).await
}

/// The typed route `CONNECT {path}/wt` serving `server`'s procedures over WebTransport.
pub fn endpoint(server: RpcServer, config: RpcWebTransportConfig) -> Endpoint<DynEnv, Public> {
  let path = format!("{}{PATH}", server.path().trim_end_matches('/'));
  let sessions = config.session.clone();
  let shared = Arc::new(Shared { server, config });
  let handler = move |env: &mut DynEnv, caps: RequestCaps, session: WebTransportSession| {
    // `admit` ran first and recorded the head; without it (never) the session just ends, so no
    // call runs its guards against an empty head.
    let head = caps
      .get::<ConnectHead>()
      .map(|head| ConnectHead::clone(&head));
    let env = caps.overlay(env);
    let shared = Arc::clone(&shared);
    from_async(move || async move {
      if let Some(head) = head {
        serve_session(shared, env, head, session).await;
      }
      Ok::<_, Never>(())
    })
  };
  wt::endpoint(path, sessions, handler)
    .layer(axum::middleware::from_fn(admit))
    .public()
}

/// Serve one already established session (an endpoint of your own): `head` is the request head
/// the procedures' guards see, `env` the calls' environment, `codec` the session's codec.
pub fn serve<R>(
  server: &RpcServer,
  config: &RpcWebTransportConfig,
  env: DynEnv,
  head: Parts,
  codec: CodecId,
  session: WebTransportSession,
) -> Effect<(), Never, R>
where
  R: Send + Sync + 'static,
{
  let shared = Arc::new(Shared {
    server: server.clone(),
    config: config.clone(),
  });
  let head = ConnectHead {
    head: Arc::new(head),
    codec,
  };
  from_async(move || async move {
    serve_session(shared, env, head, session).await;
    Ok(())
  })
}

// --- frames on streams ------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct Peek {
  id: u64,
  kind: FrameKind,
}

/// The call id and kind of an encoded frame.
fn peek(codec: CodecId, bytes: &[u8]) -> Option<(u64, FrameKind)> {
  match codec {
    CodecId::Json => serde_json::from_slice::<Peek>(bytes)
      .ok()
      .map(|p| (p.id, p.kind)),
    CodecId::Postcard => crate::codec::postcard::decode_frame(bytes)
      .ok()
      .map(|f| (f.id.get(), f.kind)),
  }
}

/// The record of the server's `ping`.
fn ping_record(codec: CodecId) -> Option<Bytes> {
  let frame = Frame::new(CallId::CONNECTION, FrameKind::Ping);
  let bytes = match codec {
    CodecId::Json => super::frame::encode(&frame, codec).ok()?,
    CodecId::Postcard => {
      crate::codec::postcard::encode_server_frame(&ProcedureCodecs::default(), &frame).ok()?
    }
  };
  encode_record(&bytes).ok().map(Bytes::from)
}

/// Reads more of `recv` into `buf`; 0 at the end of the stream.
async fn read_some(recv: &mut RecvHalf, buf: &mut Vec<u8>) -> std::io::Result<usize> {
  let mut chunk = [0_u8; 8192];
  let n = poll_fn(|cx| {
    let mut read = ReadBuf::new(&mut chunk);
    Pin::new(&mut *recv)
      .poll_read(cx, &mut read)
      .map_ok(|()| read.filled().len())
  })
  .await?;
  buf.extend_from_slice(&chunk[..n]);
  Ok(n)
}

/// The next record of a stream; `Ok(None)` at a clean end.
async fn read_record(recv: &mut RecvHalf, buf: &mut Vec<u8>) -> Result<Option<Bytes>, String> {
  loop {
    match split_record(buf) {
      Err(err) => return Err(err.to_string()),
      Ok(Some((record, used))) => {
        let record = Bytes::copy_from_slice(record);
        buf.drain(..used);
        return Ok(Some(record));
      }
      Ok(None) => {}
    }
    match read_some(recv, buf).await {
      Ok(0) if buf.is_empty() => return Ok(None),
      Ok(0) => return Err("the stream ended inside a record".to_owned()),
      Ok(_) => {}
      // A reset stream: its call is the client's to cancel.
      Err(_) => return Ok(None),
    }
  }
}

async fn write_all(send: &mut SendHalf, mut bytes: &[u8]) -> std::io::Result<()> {
  while !bytes.is_empty() {
    let n = poll_fn(|cx| Pin::new(&mut *send).poll_write(cx, bytes)).await?;
    if n == 0 {
      return Err(std::io::ErrorKind::WriteZero.into());
    }
    bytes = &bytes[n..];
  }
  Ok(())
}

/// Writes the records queued for one stream, then finishes it.
async fn write_stream(mut send: SendHalf, mut records: mpsc::Receiver<Bytes>) {
  while let Some(record) = records.recv().await {
    if write_all(&mut send, &record).await.is_err() {
      return;
    }
  }
  let _ = poll_fn(|cx| Pin::new(&mut send).poll_shutdown(cx)).await;
}

// --- the session ------------------------------------------------------------------------------

/// Where the server's frames go: the control stream, and the stream of each call that has one.
struct Routes {
  control: Option<u64>,
  /// Taken by the control stream's writer once it is open.
  control_rx: Option<mpsc::Receiver<Bytes>>,
  calls: HashMap<u64, (u64, mpsc::Sender<Bytes>)>,
}

/// What the stream tasks share with the session loop.
struct Mux {
  codec: CodecId,
  routes: Mutex<Routes>,
  control_tx: mpsc::Sender<Bytes>,
  /// A record broke the rules: why (the session loop closes the session).
  violations: mpsc::Sender<String>,
  /// Milliseconds since `start` at the last record from the client.
  seen: AtomicU64,
  start: Instant,
  /// Most call bindings at once (streams whose call never exits).
  max_bindings: usize,
}

impl Mux {
  fn routes(&self) -> std::sync::MutexGuard<'_, Routes> {
    self.routes.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn touch(&self) {
    let now = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
    self.seen.store(now, Ordering::Release);
  }

  fn silent_for(&self) -> Duration {
    let seen = Duration::from_millis(self.seen.load(Ordering::Acquire));
    self.start.elapsed().saturating_sub(seen)
  }

  async fn violation(&self, why: String) {
    let _ = self.violations.send(why).await;
  }

  /// Binds the stream `key` to the id of its first record: the control stream (id 0) or a
  /// call's stream (its writer spawned on `tasks`). `Err` when the rules forbid it.
  fn bind(
    &self,
    key: u64,
    id: u64,
    send: SendHalf,
    tasks: &Mutex<JoinSet<()>>,
  ) -> Result<(), String> {
    let mut routes = self.routes();
    let writer = if id == 0 {
      let Some(rx) = routes.control_rx.take() else {
        return Err("a second control stream".to_owned());
      };
      routes.control = Some(key);
      write_stream(send, rx)
    } else {
      if routes.calls.len() >= self.max_bindings {
        return Err("too many call streams".to_owned());
      }
      let (tx, rx) = mpsc::channel(STREAM_QUEUE);
      // A resumed call moving to this stream replaces its old route.
      routes.calls.insert(id, (key, tx));
      write_stream(send, rx)
    };
    drop(routes);
    tasks
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .spawn(writer);
    Ok(())
  }

  /// The queue of the stream that carries `id`'s frames.
  fn route(&self, id: u64) -> mpsc::Sender<Bytes> {
    let routes = self.routes();
    match routes.calls.get(&id) {
      Some((_, tx)) if id != 0 => tx.clone(),
      _ => self.control_tx.clone(),
    }
  }
}

/// One stream from the client: bind it by its first record, then hand its records on.
async fn read_stream(
  mux: Arc<Mux>,
  key: u64,
  stream: WebTransportStream,
  inbox: mpsc::Sender<WsMessage>,
  tasks: Arc<Mutex<JoinSet<()>>>,
) {
  let WebTransportStream { send, mut recv } = stream;
  let mut send = Some(send);
  let mut bound: Option<u64> = None;
  let mut buf = Vec::new();
  loop {
    let record = match read_record(&mut recv, &mut buf).await {
      Ok(Some(record)) => record,
      Ok(None) => return,
      Err(why) => return mux.violation(why).await,
    };
    mux.touch();
    let Some((id, _)) = peek(mux.codec, &record) else {
      return mux.violation("an undecodable frame".to_owned()).await;
    };
    match bound {
      None => {
        let send = send.take().expect("unbound streams hold their send half");
        if let Err(why) = mux.bind(key, id, send, &tasks) {
          return mux.violation(why).await;
        }
        bound = Some(id);
      }
      // The control stream carries any call's frames; a call stream only its own.
      Some(0) => {}
      Some(own) if own == id => {}
      Some(own) => {
        let why = format!("a frame for call {id} on the stream of call {own}");
        return mux.violation(why).await;
      }
    }
    if inbox.send(WsMessage::Binary(record)).await.is_err() {
      return;
    }
  }
}

/// The connection logic's frames, to the streams that carry them; a call's stream finishes after
/// its `exit`.
async fn route_outbox(mux: Arc<Mux>, mut outbox: mpsc::Receiver<WsMessage>) {
  while let Some(msg) = outbox.recv().await {
    let bytes = msg.as_bytes();
    let (Some((id, kind)), Ok(record)) = (peek(mux.codec, bytes), encode_record(bytes)) else {
      continue;
    };
    let record = Bytes::from(record);
    let tx = mux.route(id);
    if let Err(mpsc::error::SendError(record)) = tx.send(record).await {
      // The call's stream is gone: the control stream carries the rest.
      mux.routes().calls.remove(&id);
      let _ = mux.control_tx.send(record).await;
    }
    if kind == FrameKind::Exit && id != 0 {
      mux.routes().calls.remove(&id);
    }
  }
}

async fn serve_session(
  shared: Arc<Shared>,
  env: DynEnv,
  head: ConnectHead,
  mut session: WebTransportSession,
) {
  let ConnectHead { head, codec } = head;
  let config = &shared.config;
  let (control_tx, control_rx) = mpsc::channel(CONTROL_QUEUE);
  let (violations, mut violated) = mpsc::channel(1);
  let mux = Arc::new(Mux {
    codec,
    routes: Mutex::new(Routes {
      control: None,
      control_rx: Some(control_rx),
      calls: HashMap::new(),
    }),
    control_tx,
    violations,
    seen: AtomicU64::new(0),
    start: Instant::now(),
    max_bindings: config
      .rpc
      .calls_limit()
      .saturating_mul(2)
      .saturating_add(16),
  });
  let (in_tx, in_rx) = mpsc::channel(INBOX);
  let (out_tx, out_rx) = mpsc::channel(OUTBOX);
  let socket = WsSocket::from_channels(
    in_rx,
    out_tx,
    Some(codec.subprotocol().to_owned()),
    session.client_ip(),
  );
  let head = Parts::clone(&head);
  let conn = ws::serve_socket_on::<id_effect::HNil>(
    &shared.server,
    &config.rpc,
    env,
    head,
    codec,
    socket,
    TRANSPORT,
  );
  let mut conn = std::pin::pin!(EffectExecutor::global().run_effect(conn));
  let tasks = Arc::new(Mutex::new(JoinSet::new()));
  tasks
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .spawn(route_outbox(Arc::clone(&mux), out_rx));
  let mut tick = tokio::time::interval(config.heartbeat);
  tick.tick().await;
  let deadline = config.heartbeat * config.missed;
  let ping = ping_record(codec);
  let mut keys = 0_u64;
  let mut ended = false;
  loop {
    tokio::select! {
      stream = session.accept_bi() => match stream {
        Some(stream) => {
          keys += 1;
          let reader = read_stream(
            Arc::clone(&mux),
            keys,
            stream,
            in_tx.clone(),
            Arc::clone(&tasks),
          );
          tasks.lock().unwrap_or_else(PoisonError::into_inner).spawn(reader);
        }
        None => break,
      },
      Some(why) = violated.recv() => {
        tracing::debug!(target: TARGET, %why, "rpc webtransport session closed on a protocol violation");
        session.close(CLOSE_PROTOCOL, &why);
        break;
      }
      _ = tick.tick() => {
        let no_control = mux.routes().control.is_none();
        let dead = mux.silent_for() >= deadline
          || (no_control && mux.start.elapsed() >= deadline);
        if dead {
          tracing::debug!(target: TARGET, "rpc webtransport session closed: no heartbeat");
          session.close(CLOSE_OK, "heartbeat");
          break;
        }
        if let Some(ping) = &ping {
          let _ = mux.control_tx.try_send(ping.clone());
        }
      }
      _ = &mut conn => {
        ended = true;
        break;
      }
    }
  }
  // The streams stop; the connection logic sees its inbox end and lets resumable calls detach.
  tasks
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .abort_all();
  drop(in_tx);
  drop(session);
  if !ended {
    let _ = conn.await;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn codecs_come_from_the_query() {
    assert_eq!(query_codec(None), Ok(CodecId::Json));
    assert_eq!(query_codec(Some("a=1")), Ok(CodecId::Json));
    assert_eq!(
      query_codec(Some("a=1&codec=idrpc.postcard.v1")),
      Ok(CodecId::Postcard)
    );
    assert_eq!(query_codec(Some("codec=idrpc.json.v1")), Ok(CodecId::Json));
    assert_eq!(query_codec(Some("codec=cbor")), Err("cbor".to_owned()));
  }

  #[test]
  fn frames_peek_and_ping_in_both_codecs() {
    let frame = Frame::new(CallId::new(7).unwrap(), FrameKind::Exit);
    let json = super::super::frame::encode(&frame, CodecId::Json).unwrap();
    assert_eq!(peek(CodecId::Json, &json), Some((7, FrameKind::Exit)));
    assert_eq!(peek(CodecId::Json, b"nope"), None);
    assert_eq!(peek(CodecId::Postcard, b"\xff"), None);
    for codec in [CodecId::Json, CodecId::Postcard] {
      let ping = ping_record(codec).unwrap();
      let (record, used) = split_record(&ping).unwrap().unwrap();
      assert_eq!(used, ping.len());
      assert_eq!(peek(codec, record), Some((0, FrameKind::Ping)));
    }
  }

  #[test]
  fn config_defaults_and_clamps() {
    let config = RpcWebTransportConfig::new();
    assert_eq!(config.heartbeat_settings(), (DEFAULT_HEARTBEAT, 2));
    assert!(!config.session_config().origin_policy().is_any());
    assert_eq!(config.rpc_config().calls_limit(), ws::DEFAULT_MAX_CALLS);
    let config = config.heartbeat(Duration::ZERO, 0);
    assert_eq!(config.heartbeat_settings(), (Duration::from_millis(1), 1));
  }
}
