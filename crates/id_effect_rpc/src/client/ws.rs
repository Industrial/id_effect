//! The WebSocket client (feature `ws`): one socket, every procedure kind multiplexed on it by
//! call id, with the server's credit backpressure honoured both ways (design note §5).
//!
//! [`WsClient::connect`] offers `idrpc.json.v1`, reads the server's `hello`, then routes frames:
//! each [`WsCall`] gets its own items and exit. A call grants the server credits back as its
//! items are read (half a window at a time), so an unread stream stops its producer on the server;
//! [`WsCall::send`] waits for credits the server granted. Dropping a [`WsCall`] before its exit
//! cancels the call; dropping the last handle to the socket closes it.
//!
//! **`wss://` (ADR 0013 R8).** [`WsClient::connect`] dials `wss://` itself over rustls: the
//! server name is checked and the server certificate verified by the platform verifier
//! (`rustls-platform-verifier`, the root source of the platform HTTP client: the OS trust store
//! on Windows and macOS, the system roots elsewhere), plus the private CAs a
//! [`WsClientConfig::add_root_certificate_der`] adds. ALPN is `http/1.1`. There is no way to
//! accept an untrusted or invalid certificate. [`WsClient::over`] runs the protocol on a socket
//! you opened yourself (a custom stream, an in-memory pipe).
//!
//! **Codecs.** JSON (`idrpc.json.v1`) by default. With feature `postcard`,
//! [`WsClientConfig::postcard`] offers `idrpc.postcard.v1` instead: every frame is one binary
//! message, and since postcard is not self-describing the client encodes inputs and decodes
//! outputs with the typed codecs (`WireSchema::with_serde`) of the service's procedures, taken
//! from the [`RpcGroup`](crate::RpcGroup) it is given (its handlers never run here). A procedure
//! the group lacks is refused (404) and one without typed codecs too (415), before anything is
//! sent. The API stays JSON values either way.
//!
//! ```rust,no_run
//! # async fn demo(ca_der: Vec<u8>) -> Result<(), id_effect_rpc::RpcClientError> {
//! use id_effect_rpc::client::ws::{WsClient, WsClientConfig};
//!
//! // The platform roots.
//! let public = WsClient::connect("wss://api.example.com/rpc/ws").await?;
//! // The platform roots and a private CA; a request carries headers too.
//! let config = WsClientConfig::new().add_root_certificate_der(ca_der);
//! let private = WsClient::connect_with("wss://internal.example/rpc/ws", &config).await?;
//! # drop((public, private));
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{Connector, WebSocketStream};

use super::{CallEvent, RpcClientError, exit_result};
use crate::codec::CodecId;
use crate::service::{
  BoxFuture, FrameStream, ItemReceiver, RpcTransport, TransportCall, TransportError,
};
use crate::transport::{REJECTED_TAG, frame};
use crate::wire::{CallId, Cursor, Frame, FrameFlags, FrameKind, Hello, WireExit};

/// The most credits held for one call (grants beyond are clamped).
const MAX_GRANTED: usize = 1 << 20;

fn transport(err: impl std::fmt::Display) -> RpcClientError {
  RpcClientError::Transport(err.to_string())
}

fn closed() -> RpcClientError {
  RpcClientError::Transport("the rpc websocket closed".to_owned())
}

/// The message of a client frame: JSON text, or (a postcard call) a binary postcard frame.
fn message(frame: &Frame, codec: &CallCodec) -> Result<Message, RpcClientError> {
  #[cfg(feature = "postcard")]
  if let Some(bridges) = &codec.bridges {
    return bridges.message(frame);
  }
  #[cfg(not(feature = "postcard"))]
  let _ = codec;
  let bytes = frame::encode(frame, CodecId::Json).map_err(transport)?;
  Ok(Message::Text(
    String::from_utf8_lossy(&bytes).into_owned().into(),
  ))
}

// ---------------------------------------------------------------------------------------------
// Configuration and TLS
// ---------------------------------------------------------------------------------------------

/// How [`WsClient::connect_with`] connects: the CA roots trusted for `wss://` on top of the
/// platform roots, and the codec offered.
///
/// The default is the platform roots only and JSON. No setting accepts an untrusted or invalid
/// server certificate.
#[derive(Clone, Default)]
pub struct WsClientConfig {
  root_certificates: Vec<Vec<u8>>,
  #[cfg(feature = "postcard")]
  postcard: Option<Arc<crate::RpcGroup>>,
}

impl std::fmt::Debug for WsClientConfig {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("WsClientConfig")
      .field("root_certificates", &self.root_certificates.len())
      .field("codec", &self.codec())
      .finish()
  }
}

impl WsClientConfig {
  /// The platform roots only, JSON.
  pub fn new() -> Self {
    Self::default()
  }

  /// Trusts the CA certificate `der` (DER) for `wss://`, in addition to the platform roots (a
  /// private CA), as the platform HTTP client's `HttpClientConfig::add_root_certificate_der`.
  /// The certificate is parsed when a connection is made; one that does not parse fails it.
  #[must_use]
  pub fn add_root_certificate_der(mut self, der: impl Into<Vec<u8>>) -> Self {
    self.root_certificates.push(der.into());
    self
  }

  /// The extra CA roots (DER), in the order they were added.
  pub fn root_certificates(&self) -> &[Vec<u8>] {
    &self.root_certificates
  }

  /// Offers `idrpc.postcard.v1` (feature `postcard`) and transcodes with the typed codecs of
  /// `schemas`, the service's own group (its handlers never run here). A server that does not
  /// serve postcard refuses the upgrade (415); there is no fallback to JSON.
  #[cfg(feature = "postcard")]
  #[must_use]
  pub fn postcard(mut self, schemas: crate::RpcGroup) -> Self {
    self.postcard = Some(Arc::new(schemas));
    self
  }

  /// The codec offered.
  pub fn codec(&self) -> CodecId {
    self.wire().codec()
  }

  fn wire(&self) -> Wire {
    #[cfg(feature = "postcard")]
    if let Some(schemas) = &self.postcard {
      return Wire::Postcard(Arc::clone(schemas));
    }
    Wire::Json
  }
}

/// The crypto provider: the process default if one is installed, else aws-lc-rs (as the
/// platform's TLS configs).
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
  rustls::crypto::CryptoProvider::get_default()
    .cloned()
    .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

/// The platform verifier, trusting the `extra` CA roots too.
fn verifier(
  extra: &[Vec<u8>],
  provider: Arc<rustls::crypto::CryptoProvider>,
) -> Result<rustls_platform_verifier::Verifier, rustls::Error> {
  let extra = extra
    .iter()
    .map(|der| rustls::pki_types::CertificateDer::from(der.clone()));
  #[cfg(not(target_os = "android"))]
  {
    rustls_platform_verifier::Verifier::new_with_extra_roots(extra, provider)
  }
  #[cfg(target_os = "android")]
  {
    if extra.len() > 0 {
      return Err(rustls::Error::General(
        "extra CA roots are not supported on Android".to_owned(),
      ));
    }
    rustls_platform_verifier::Verifier::new(provider)
  }
}

/// The rustls client config of `wss://` connections trusting the platform roots and `extra`.
fn build_tls(extra: &[Vec<u8>]) -> Result<Arc<rustls::ClientConfig>, String> {
  let provider = provider();
  let verifier =
    verifier(extra, Arc::clone(&provider)).map_err(|e| format!("wss trust roots: {e}"))?;
  let mut config = rustls::ClientConfig::builder_with_provider(provider)
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("wss tls: {e}"))?
    // A custom verifier is how rustls takes the platform verifier; it verifies, it does not
    // weaken anything.
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(verifier))
    .with_no_client_auth();
  config.alpn_protocols = vec![b"http/1.1".to_vec()];
  Ok(Arc::new(config))
}

/// [`build_tls`], the platform-roots-only config built once per process.
fn tls_config(extra: &[Vec<u8>]) -> Result<Arc<rustls::ClientConfig>, RpcClientError> {
  static PLATFORM: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
  if extra.is_empty() {
    PLATFORM.get_or_init(|| build_tls(&[])).clone()
  } else {
    build_tls(extra)
  }
  .map_err(RpcClientError::Transport)
}

// ---------------------------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------------------------

/// The codec of a connection.
#[derive(Clone)]
enum Wire {
  Json,
  #[cfg(feature = "postcard")]
  Postcard(Arc<crate::RpcGroup>),
}

impl Wire {
  fn codec(&self) -> CodecId {
    match self {
      Self::Json => CodecId::Json,
      #[cfg(feature = "postcard")]
      Self::Postcard(_) => CodecId::Postcard,
    }
  }

  /// The codec of a call of `tag` at `v`: JSON needs nothing, postcard the procedure's bridges.
  fn call_codec(&self, tag: &str, v: Option<u32>) -> Result<CallCodec, RpcClientError> {
    match self {
      Self::Json => {
        let _ = (tag, v);
        Ok(CallCodec::default())
      }
      #[cfg(feature = "postcard")]
      Self::Postcard(schemas) => Ok(CallCodec {
        bridges: Some(Arc::new(postcard_wire::Bridges::of(schemas, tag, v)?)),
      }),
    }
  }

  /// The server's `hello` from the first message.
  fn hello(&self, msg: &Message) -> Option<Hello> {
    match (self, msg) {
      (Self::Json, Message::Text(text)) => frame::decode(text.as_bytes(), CodecId::Json)
        .ok()
        .filter(|f| f.kind == FrameKind::Hello)
        .and_then(|f| serde_json::from_value::<Hello>(f.payload?).ok()),
      #[cfg(feature = "postcard")]
      (Self::Postcard(_), Message::Binary(bytes)) => postcard_wire::hello(bytes),
      _ => None,
    }
  }
}

/// How one call's frames are encoded: JSON, or postcard through the procedure's bridges.
#[derive(Clone, Default)]
struct CallCodec {
  #[cfg(feature = "postcard")]
  bridges: Option<Arc<postcard_wire::Bridges>>,
}

/// One inbound message, decoded.
enum Inbound {
  /// A frame to route.
  Frame(Frame),
  /// A frame of a call this side already dropped (postcard; JSON routes those as frames).
  #[cfg_attr(not(feature = "postcard"), allow(dead_code))]
  Stale,
  /// The server broke the protocol.
  Broken,
}

#[cfg(feature = "postcard")]
mod postcard_wire {
  //! Postcard frames of the client (`idrpc.postcard.v1`, one binary message per frame).

  use serde_json::Value;
  use tokio_tungstenite::tungstenite::Message;

  use super::{RpcClientError, transport};
  use crate::ProcedureKind;
  use crate::codec::postcard::{Postcard, decode_exit, decode_frame, encode_frame};
  use crate::codec::{Codec as _, CodecId, TypedCodec};
  use crate::server::dispatch;
  use crate::wire::{Frame, FrameKind, Hello, WireExit};

  const PC: CodecId = CodecId::Postcard;

  /// The typed codecs of one procedure.
  pub(super) struct Bridges {
    input: TypedCodec,
    output: TypedCodec,
    /// The kind sends items, so its `call` carries no input.
    items_in: bool,
    /// The kind sends items, so its successful `exit` is `null` (no bytes).
    unit_exit: bool,
  }

  fn refused(status: axum::http::StatusCode, detail: String) -> RpcClientError {
    let problem = id_effect_axum::route::Problem::new(status).with_detail(detail);
    RpcClientError::Refused(Box::new(problem))
  }

  impl Bridges {
    /// The bridges of `tag` at `v` in `schemas`: 404 when the group lacks the procedure, 415
    /// when it has no typed codecs.
    pub(super) fn of(
      schemas: &crate::RpcGroup,
      tag: &str,
      v: Option<u32>,
    ) -> Result<Self, RpcClientError> {
      let entry = dispatch::resolve(schemas, tag, v).map_err(|_| {
        refused(
          axum::http::StatusCode::NOT_FOUND,
          format!("`{tag}` is not in this client's schemas"),
        )
      })?;
      let codecs = entry.codecs();
      let (Some(input), Some(output)) = (codecs.input(), codecs.output()) else {
        return Err(refused(
          axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
          format!("`{tag}` has no typed codecs for postcard (`WireSchema::with_serde`)"),
        ));
      };
      let kind = entry.kind();
      Ok(Self {
        input: input.clone(),
        output: output.clone(),
        items_in: matches!(kind, ProcedureKind::ClientStream | ProcedureKind::Bidi),
        unit_exit: matches!(
          kind,
          ProcedureKind::ServerStream | ProcedureKind::Bidi | ProcedureKind::Subscription
        ),
      })
    }

    /// A client frame as a binary postcard message: a `call`'s input and an `item` through the
    /// input bridge; the other kinds carry no payload.
    pub(super) fn message(&self, frame: &Frame) -> Result<Message, RpcClientError> {
      let payload = match (frame.kind, &frame.payload) {
        (FrameKind::Call, Some(_)) if self.items_in => None,
        (FrameKind::Call | FrameKind::Item, Some(value)) => {
          Some(self.input.encode(PC, value).map_err(transport)?)
        }
        _ => None,
      };
      let bytes = encode_frame(&Frame {
        id: frame.id,
        kind: frame.kind,
        flags: frame.flags,
        tag: frame.tag.clone(),
        v: frame.v,
        cursor: frame.cursor,
        credits: frame.credits,
        payload,
      })
      .map_err(transport)?;
      Ok(Message::Binary(bytes.into()))
    }

    /// A server frame of this call with its payload as the JSON value the API hands out: an
    /// `item` through the output bridge, an `exit` as its [`WireExit`] JSON. `None` when the
    /// payload is not of its type (the server broke the protocol).
    pub(super) fn decode(&self, raw: Frame<Vec<u8>>) -> Option<Frame> {
      let payload = match (raw.kind, raw.payload.as_deref()) {
        (FrameKind::Item, Some(bytes)) => Some(self.output.decode(PC, bytes).ok()?),
        (FrameKind::Exit, Some(bytes)) => {
          let exit: WireExit = match decode_exit(bytes).ok()? {
            WireExit::Success { .. } if self.unit_exit => WireExit::Success { value: Value::Null },
            WireExit::Success { value } => WireExit::Success {
              value: self.output.decode(PC, &value).ok()?,
            },
            WireExit::Failure { error } => WireExit::Failure { error },
          };
          Some(serde_json::to_value(exit).ok()?)
        }
        _ => None,
      };
      Some(Frame {
        id: raw.id,
        kind: raw.kind,
        flags: raw.flags,
        tag: raw.tag,
        v: raw.v,
        cursor: raw.cursor,
        credits: raw.credits,
        payload,
      })
    }
  }

  /// The envelope of a server frame (its payload still postcard bytes).
  pub(super) fn envelope(bytes: &[u8]) -> Option<Frame<Vec<u8>>> {
    decode_frame(bytes).ok()
  }

  /// The server's `hello` frame.
  pub(super) fn hello(bytes: &[u8]) -> Option<Hello> {
    let frame = decode_frame(bytes)
      .ok()
      .filter(|f| f.kind == FrameKind::Hello)?;
    Postcard::decode::<Hello>(frame.payload.as_deref()?).ok()
  }
}

/// One open call's mailbox.
struct Slot {
  frames: mpsc::Sender<Frame>,
  credits: Arc<Semaphore>,
  /// Read by the postcard reader to decode this call's frames.
  #[cfg_attr(not(feature = "postcard"), allow(dead_code))]
  codec: CallCodec,
}

struct Shared {
  out: mpsc::Sender<Message>,
  calls: Mutex<HashMap<CallId, Slot>>,
  next_id: AtomicU64,
  closed: AtomicBool,
  hello: Hello,
  wire: Wire,
  tasks: [JoinHandle<()>; 2],
}

impl Shared {
  fn calls(&self) -> std::sync::MutexGuard<'_, HashMap<CallId, Slot>> {
    self.calls.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Decode one message from the server.
  fn decode(&self, bytes: &[u8]) -> Inbound {
    match &self.wire {
      Wire::Json => match frame::decode(bytes, CodecId::Json) {
        Ok(frame) => Inbound::Frame(frame),
        Err(_) => Inbound::Broken,
      },
      #[cfg(feature = "postcard")]
      Wire::Postcard(_) => {
        let Some(raw) = postcard_wire::envelope(bytes) else {
          return Inbound::Broken;
        };
        let bridges = self
          .calls()
          .get(&raw.id)
          .and_then(|slot| slot.codec.bridges.clone());
        match bridges {
          None => Inbound::Stale,
          Some(bridges) => bridges.decode(raw).map_or(Inbound::Broken, Inbound::Frame),
        }
      }
    }
  }
}

impl Drop for Shared {
  fn drop(&mut self) {
    for task in &self.tasks {
      task.abort();
    }
  }
}

/// A connected RPC WebSocket. Cheap to clone; the socket closes when the last clone and the
/// last [`WsCall`] are gone.
#[derive(Clone)]
pub struct WsClient {
  shared: Arc<Shared>,
}

impl std::fmt::Debug for WsClient {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("WsClient")
      .field("hello", &self.shared.hello)
      .finish_non_exhaustive()
  }
}

impl WsClient {
  /// Connect to `url` (`ws://host/rpc/ws` or `wss://host/rpc/ws`, see
  /// [`RpcClientConfig::ws_url`](super::RpcClientConfig::ws_url)) with the platform roots and
  /// JSON, and read the `hello` ([`WsClient::connect_with`] with the default
  /// [`WsClientConfig`]).
  ///
  /// # Errors
  ///
  /// As [`WsClient::connect_with`].
  pub async fn connect(url: &str) -> Result<Self, RpcClientError> {
    Self::connect_with(url, &WsClientConfig::default()).await
  }

  /// Connect with `config`: `request` is a URL or a request carrying headers (an
  /// `authorization` the procedures' guards read on the upgrade); its
  /// `sec-websocket-protocol` is replaced by the codec `config` offers. A `wss://` URL is
  /// dialled over rustls with the server name checked and the certificate verified against
  /// the platform roots plus `config`'s extra roots.
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`]: the URL is not `ws://` or `wss://`, the TLS handshake failed
  /// (an untrusted or invalid server certificate, a name mismatch), the upgrade failed or the
  /// server refused it (its status), or the first frame was not a `hello`.
  pub async fn connect_with<R>(request: R, config: &WsClientConfig) -> Result<Self, RpcClientError>
  where
    R: IntoClientRequest + Unpin,
  {
    let mut request = request.into_client_request().map_err(transport)?;
    let wire = config.wire();
    request.headers_mut().insert(
      "sec-websocket-protocol",
      HeaderValue::from_static(wire.codec().subprotocol()),
    );
    let connector = if request.uri().scheme_str() == Some("wss") {
      Connector::Rustls(tls_config(&config.root_certificates)?)
    } else {
      Connector::Plain
    };
    // tungstenite checks the server selected the one subprotocol offered.
    let (socket, _response) =
      tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
        .await
        .map_err(transport)?;
    Self::start(socket, wire).await
  }

  /// Run the RPC protocol on an open socket (one whose handshake offered `idrpc.json.v1`): a
  /// custom stream, an in-memory pipe, a TLS stream set up by hand.
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`] when the first frame is not a `hello`.
  pub async fn over<S>(socket: WebSocketStream<S>) -> Result<Self, RpcClientError>
  where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
  {
    Self::start(socket, Wire::Json).await
  }

  /// [`WsClient::over`] for a socket whose handshake negotiated `idrpc.postcard.v1` (feature
  /// `postcard`), transcoding with the typed codecs of `schemas` as
  /// [`WsClientConfig::postcard`].
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`] when the first frame is not a postcard `hello`.
  #[cfg(feature = "postcard")]
  pub async fn over_postcard<S>(
    socket: WebSocketStream<S>,
    schemas: crate::RpcGroup,
  ) -> Result<Self, RpcClientError>
  where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
  {
    Self::start(socket, Wire::Postcard(Arc::new(schemas))).await
  }

  async fn start<S>(socket: WebSocketStream<S>, wire: Wire) -> Result<Self, RpcClientError>
  where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
  {
    let (mut sink, mut stream) = socket.split();
    let hello = match stream.next().await {
      Some(Ok(msg)) => wire.hello(&msg),
      _ => None,
    }
    .ok_or_else(|| transport("the server did not greet with a hello frame"))?;
    let (out, mut outbox) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
      // After the socket broke, sends fail at once until the client is gone (the task is then
      // aborted with the shared state).
      while let Some(msg) = outbox.recv().await {
        let _ = sink.send(msg).await;
      }
    });
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Arc<Shared>>();
    let reader = tokio::spawn(async move {
      // The reader holds the shared state weakly: the socket closes when the client is gone.
      let Ok(shared) = ready_rx.await else { return };
      let weak = Arc::downgrade(&shared);
      drop(shared);
      while let Some(Ok(msg)) = stream.next().await {
        let bytes = match &msg {
          Message::Text(text) => text.as_bytes(),
          Message::Binary(bytes) => bytes.as_ref(),
          Message::Close(_) => break,
          _ => continue,
        };
        let Some(shared) = weak.upgrade() else { return };
        let routed = match shared.decode(bytes) {
          Inbound::Frame(frame) => route(&shared, frame),
          Inbound::Stale => true,
          Inbound::Broken => false,
        };
        if !routed {
          break;
        }
      }
      if let Some(shared) = weak.upgrade() {
        shared.closed.store(true, Ordering::Release);
        shared.calls().clear();
      }
    });
    let shared = Arc::new(Shared {
      out,
      calls: Mutex::new(HashMap::new()),
      next_id: AtomicU64::new(1),
      closed: AtomicBool::new(false),
      hello,
      wire,
      tasks: [reader, writer],
    });
    let _ = ready_tx.send(Arc::clone(&shared));
    Ok(Self { shared })
  }

  /// The server's `hello`: session id, codec, frame cap, credit window.
  pub fn hello(&self) -> &Hello {
    &self.shared.hello
  }

  /// The codec of this connection.
  pub fn codec(&self) -> CodecId {
    self.shared.wire.codec()
  }

  /// Open a call of `tag`: `payload` is the input (`null` for a client stream or a two-way
  /// stream, which send items), `after` the cursor a subscription resumes after.
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`] once the socket is closed.
  pub async fn open(
    &self,
    tag: &str,
    payload: Value,
    after: Option<Cursor>,
  ) -> Result<WsCall, RpcClientError> {
    self.open_version(tag, None, payload, after).await
  }

  /// [`WsClient::open`] at service version `v` (`None`: the newest the server serves).
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`] once the socket is closed, or when the input does not
  /// encode; over postcard, [`RpcClientError::Refused`] (404) when the client's schemas lack
  /// the procedure and (415) when it has no typed codecs.
  pub async fn open_version(
    &self,
    tag: &str,
    v: Option<u32>,
    payload: Value,
    after: Option<Cursor>,
  ) -> Result<WsCall, RpcClientError> {
    let shared = &self.shared;
    if shared.closed.load(Ordering::Acquire) {
      return Err(closed());
    }
    let codec = shared.wire.call_codec(tag, v)?;
    let n = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let id = CallId::new(n).map_err(transport)?;
    let mut call = Frame::call(id, tag, payload);
    call.cursor = after;
    call.v = v;
    let call_msg = message(&call, &codec)?;
    let window = shared.hello.window as usize;
    let (tx, rx) = mpsc::channel(window + 1);
    let credits = Arc::new(Semaphore::new(window));
    shared.calls().insert(
      id,
      Slot {
        frames: tx,
        credits: Arc::clone(&credits),
        codec: codec.clone(),
      },
    );
    let call = WsCall {
      id,
      frames: rx,
      credits,
      shared: Arc::clone(shared),
      codec,
      read: 0,
      done: false,
    };
    shared.out.send(call_msg).await.map_err(|_| closed())?;
    Ok(call)
  }

  /// Call a query or mutation: its output, or its failure.
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Failure`] (typed, reserved or `Rejected`), or
  /// [`RpcClientError::Transport`].
  pub async fn call(&self, tag: &str, payload: Value) -> Result<Value, RpcClientError> {
    self.open(tag, payload, None).await?.finish().await
  }

  /// Call a client stream with `items` (sent as credits allow): its output.
  ///
  /// # Errors
  ///
  /// As [`WsClient::call`].
  pub async fn client_stream(
    &self,
    tag: &str,
    items: impl IntoIterator<Item = Value>,
  ) -> Result<Value, RpcClientError> {
    let call = self.open(tag, Value::Null, None).await?;
    for item in items {
      call.send(item).await?;
    }
    call.end().await?;
    call.finish().await
  }
}

/// Deliver one frame from the server; `false` when the server broke the protocol.
fn route(shared: &Shared, frame: Frame) -> bool {
  let mut calls = shared.calls();
  let Some(slot) = calls.get(&frame.id) else {
    // A frame for a call this side already dropped (or the connection's own).
    return true;
  };
  match frame.kind {
    FrameKind::Credit => {
      let granted = frame.credits.unwrap_or(0) as usize;
      let room = MAX_GRANTED.saturating_sub(slot.credits.available_permits());
      slot.credits.add_permits(granted.min(room));
      true
    }
    FrameKind::Item | FrameKind::Exit => {
      let (id, exit) = (frame.id, frame.kind == FrameKind::Exit);
      // At most a window of items is outstanding, so the mailbox has room unless the server
      // ignored its credits.
      let delivered = slot.frames.try_send(frame).is_ok();
      if exit {
        calls.remove(&id);
      }
      delivered
    }
    _ => true,
  }
}

/// One open call on a [`WsClient`].
pub struct WsCall {
  id: CallId,
  frames: mpsc::Receiver<Frame>,
  credits: Arc<Semaphore>,
  shared: Arc<Shared>,
  codec: CallCodec,
  read: u32,
  done: bool,
}

impl std::fmt::Debug for WsCall {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("WsCall")
      .field("id", &self.id)
      .field("done", &self.done)
      .finish_non_exhaustive()
  }
}

impl WsCall {
  /// The call id.
  pub fn id(&self) -> CallId {
    self.id
  }

  fn sink(&self) -> ItemSink {
    ItemSink {
      id: self.id,
      credits: Arc::clone(&self.credits),
      shared: Arc::clone(&self.shared),
      codec: self.codec.clone(),
    }
  }

  async fn send_frame(&self, frame: &Frame) -> Result<(), RpcClientError> {
    self.sink().send_frame(frame).await
  }

  /// The next frame from the server: `None` once the exit was read, `Some(None)` when the
  /// socket closed first. Reading an item grants the server a credit back (half a window at a
  /// time).
  async fn recv(&mut self) -> Option<Option<Frame>> {
    if self.done {
      return None;
    }
    let Some(frame) = self.frames.recv().await else {
      self.done = true;
      return Some(None);
    };
    if frame.kind == FrameKind::Exit {
      self.done = true;
    } else {
      self.read += 1;
      if self.read >= (self.shared.hello.window / 2).max(1) {
        let grant = Frame::credit(self.id, self.read);
        self.read = 0;
        let _ = self.send_frame(&grant).await;
      }
    }
    Some(Some(frame))
  }

  /// The next event: items, then one [`CallEvent::Exit`], then `None`. Reading an item grants
  /// the server a credit back (half a window at a time).
  pub async fn next(&mut self) -> Option<CallEvent> {
    Some(match self.recv().await? {
      None => CallEvent::Exit(Err(closed())),
      Some(frame) if frame.kind == FrameKind::Exit => CallEvent::Exit(exit_result(frame.payload)),
      Some(frame) => CallEvent::Item {
        value: frame.payload.unwrap_or(Value::Null),
        cursor: frame.cursor,
      },
    })
  }

  /// Send one item of a client stream or two-way stream, waiting for a credit.
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`] once the socket is closed.
  pub async fn send(&self, item: Value) -> Result<(), RpcClientError> {
    self.sink().send(item).await
  }

  /// End this side's input (half-close).
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`] once the socket is closed.
  pub async fn end(&self) -> Result<(), RpcClientError> {
    self.sink().end().await
  }

  /// Interrupt the call; its exit (`Cancelled`) still arrives.
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`] once the socket is closed.
  pub async fn cancel(&self) -> Result<(), RpcClientError> {
    self
      .send_frame(&Frame::new(self.id, FrameKind::Cancel))
      .await
  }

  /// Skip the items and wait for the exit: the output, or the failure.
  ///
  /// # Errors
  ///
  /// As [`WsClient::call`].
  pub async fn finish(mut self) -> Result<Value, RpcClientError> {
    loop {
      match self.next().await {
        Some(CallEvent::Exit(result)) => return result,
        Some(CallEvent::Item { .. }) => {}
        None => return Err(closed()),
      }
    }
  }
}

/// The sending half of a call: items against credits, the half-close.
struct ItemSink {
  id: CallId,
  credits: Arc<Semaphore>,
  shared: Arc<Shared>,
  codec: CallCodec,
}

impl ItemSink {
  async fn send_frame(&self, frame: &Frame) -> Result<(), RpcClientError> {
    let msg = message(frame, &self.codec)?;
    self.shared.out.send(msg).await.map_err(|_| closed())
  }

  async fn send(&self, item: Value) -> Result<(), RpcClientError> {
    self.credits.acquire().await.map_err(|_| closed())?.forget();
    let frame = Frame::new(self.id, FrameKind::Item).with_payload(item);
    self.send_frame(&frame).await
  }

  async fn end(&self) -> Result<(), RpcClientError> {
    let frame = Frame::new(self.id, FrameKind::End).with_flags(FrameFlags::END);
    self.send_frame(&frame).await
  }

  /// Send `items` as credits allow, then end the input.
  async fn feed(self, mut items: ItemReceiver) {
    while let Some(item) = items.recv().await {
      if self.send(item).await.is_err() {
        return;
      }
    }
    let _ = self.end().await;
  }
}

// ---------------------------------------------------------------------------------------------
// Generated clients (`service::RpcTransport`)
// ---------------------------------------------------------------------------------------------

fn io(err: RpcClientError) -> TransportError {
  match err {
    // A postcard call the client's schemas cannot carry (404, 415), refused before sending.
    RpcClientError::Refused(problem) => TransportError::Refused {
      status: problem.status.as_u16(),
      problem: Some(problem),
    },
    other => TransportError::Io(other.to_string()),
  }
}

/// The `exit` of a call that lost its socket: `Cancelled` (the server interrupts it too).
fn lost(id: CallId) -> Frame {
  let exit = serde_json::json!({"_tag": "Failure", "error": {"_tag": "Cancelled"}});
  Frame::new(id, FrameKind::Exit).with_payload(exit)
}

/// A unary call's exit; a `Rejected` exit (unknown procedure, a guard) is the refusal.
fn unary_exit(frame: &Frame) -> Result<WireExit, TransportError> {
  let exit = frame
    .payload
    .clone()
    .and_then(|payload| serde_json::from_value::<WireExit>(payload).ok())
    .ok_or_else(|| TransportError::Protocol("malformed exit".to_owned()))?;
  match &exit {
    WireExit::Failure { error } if error["_tag"] == REJECTED_TAG => Err(TransportError::Refused {
      status: error["status"]
        .as_u64()
        .and_then(|s| u16::try_from(s).ok())
        .unwrap_or(400),
      problem: None,
    }),
    _ => Ok(exit),
  }
}

impl WsCall {
  /// The next frame, an `exit` (`Cancelled`) standing in for a socket that closed first.
  async fn next_frame(&mut self) -> Option<Frame> {
    Some(self.recv().await?.unwrap_or_else(|| lost(self.id)))
  }

  /// The frames as a stream: `item`s, then one `exit`.
  fn into_frames(self) -> FrameStream {
    Box::pin(futures_util::stream::unfold(self, |mut call| async move {
      call.next_frame().await.map(|frame| (frame, call))
    }))
  }

  /// Wait for the exit, skipping items.
  async fn exit(&mut self) -> Result<WireExit, TransportError> {
    loop {
      match self.next_frame().await {
        Some(frame) if frame.kind == FrameKind::Exit => return unary_exit(&frame),
        Some(_) => {}
        None => return Err(TransportError::Protocol("the call has ended".to_owned())),
      }
    }
  }
}

/// A two-way stream's frames, feeding the client's items while they are read; dropping it stops
/// the feeding (and cancels the call).
struct Feeding {
  frames: FrameStream,
  feeder: JoinHandle<()>,
}

impl futures_util::Stream for Feeding {
  type Item = Frame;

  fn poll_next(
    mut self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Option<Frame>> {
    self.frames.as_mut().poll_next(cx)
  }
}

impl Drop for Feeding {
  fn drop(&mut self) {
    self.feeder.abort();
  }
}

/// Generated clients (`<Trait>Client<WsClient>`) call every kind over this socket. A call refused
/// before it ran is [`TransportError::Refused`] for queries, mutations and client streams; a
/// stream kind sees it as its `exit` frame (the stream is returned before the server answers).
impl RpcTransport for WsClient {
  fn unary(&self, call: TransportCall) -> BoxFuture<Result<WireExit, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let v = Some(call.version);
      let mut ws = this
        .open_version(&call.tag, v, call.payload, None)
        .await
        .map_err(io)?;
      ws.exit().await
    })
  }

  fn server_stream(&self, call: TransportCall) -> BoxFuture<Result<FrameStream, TransportError>> {
    self.subscribe(call, None)
  }

  fn client_stream(
    &self,
    call: TransportCall,
    items: ItemReceiver,
  ) -> BoxFuture<Result<WireExit, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let v = Some(call.version);
      let mut ws = this
        .open_version(&call.tag, v, Value::Null, None)
        .await
        .map_err(io)?;
      let feed = ws.sink().feed(items);
      match futures_util::future::select(Box::pin(feed), Box::pin(ws.exit())).await {
        futures_util::future::Either::Left(((), exit)) => exit.await,
        futures_util::future::Either::Right((exit, _feed)) => exit,
      }
    })
  }

  fn bidi(
    &self,
    call: TransportCall,
    items: ItemReceiver,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let v = Some(call.version);
      let ws = this
        .open_version(&call.tag, v, Value::Null, None)
        .await
        .map_err(io)?;
      let feeder = tokio::spawn(ws.sink().feed(items));
      let frames: FrameStream = Box::pin(Feeding {
        frames: ws.into_frames(),
        feeder,
      });
      Ok(frames)
    })
  }

  fn subscribe(
    &self,
    call: TransportCall,
    after: Option<Cursor>,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let v = Some(call.version);
      let ws = this
        .open_version(&call.tag, v, call.payload, after)
        .await
        .map_err(io)?;
      Ok(ws.into_frames())
    })
  }
}

impl Drop for WsCall {
  fn drop(&mut self) {
    self.shared.calls().remove(&self.id);
    if !self.done
      && let Ok(msg) = message(&Frame::new(self.id, FrameKind::Cancel), &self.codec)
    {
      let _ = self.shared.out.try_send(msg);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tls_configs_speak_http11_and_refuse_unparsable_roots() {
    let err = build_tls(&[b"not a certificate".to_vec()]).unwrap_err();
    assert!(err.contains("wss trust roots"), "{err}");
    // The platform-roots config is built once (when the platform has roots at all).
    if let (Ok(a), Ok(b)) = (tls_config(&[]), tls_config(&[])) {
      assert!(Arc::ptr_eq(&a, &b));
      assert_eq!(a.alpn_protocols, [b"http/1.1".to_vec()]);
    }
  }

  #[cfg(feature = "postcard")]
  mod postcard {
    use id_effect::{DynEnv, Never, Stream, schema, succeed};
    use serde_json::json;
    use tokio_tungstenite::tungstenite::protocol::Role;

    use super::super::*;
    use crate::codec::Codec as _;
    use crate::codec::postcard::{Postcard, decode_frame, encode_exit, encode_frame};
    use crate::wire::SessionId;
    use crate::{Procedure, RpcGroup, WireSchema};

    fn id(n: u64) -> CallId {
      CallId::new(n).unwrap()
    }

    fn group() -> RpcGroup {
      let mut group = RpcGroup::new();
      group.query(
        Procedure::query("T.len").public(),
        WireSchema::new(schema::string::<()>()).with_serde(),
        WireSchema::new(schema::i64::<()>()).with_serde(),
        |s: String, _: &mut DynEnv| succeed::<_, Never, _>(i64::try_from(s.len()).unwrap_or(0)),
      );
      group.server_stream(
        Procedure::server_stream("T.up").public(),
        WireSchema::new(schema::i64::<()>()).with_serde(),
        WireSchema::new(schema::i64::<()>()).with_serde(),
        |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
      );
      group
    }

    fn binary(msg: Message) -> Vec<u8> {
      match msg {
        Message::Binary(bytes) => bytes.to_vec(),
        other => panic!("not a binary message: {other:?}"),
      }
    }

    fn server_frame(id: CallId, kind: FrameKind, payload: Vec<u8>) -> Message {
      let frame = Frame::new(id, kind).with_payload(payload);
      Message::Binary(encode_frame(&frame).unwrap().into())
    }

    #[test]
    fn bridges_encode_inputs_and_decode_outputs() {
      let group = group();
      let len = postcard_wire::Bridges::of(&group, "T.len", None).unwrap();
      let call = binary(
        len
          .message(&Frame::call(id(1), "T.len", json!("abc")))
          .unwrap(),
      );
      let raw = decode_frame(&call).unwrap();
      assert_eq!(raw.tag.as_deref(), Some("T.len"));
      let input: String = Postcard::decode(raw.payload.as_deref().unwrap()).unwrap();
      assert_eq!(input, "abc");
      // Control frames carry no payload; an input the Schema refuses is not sent.
      let credit = binary(len.message(&Frame::credit(id(1), 2)).unwrap());
      assert_eq!(decode_frame(&credit).unwrap().payload, None);
      assert!(len.message(&Frame::call(id(1), "T.len", json!(7))).is_err());

      let raw = |kind, payload: Vec<u8>| Frame::new(id(1), kind).with_payload(payload);
      let item = len.decode(raw(FrameKind::Item, Postcard::encode(&3_i64).unwrap()));
      assert_eq!(item.unwrap().payload, Some(json!(3)));
      assert!(len.decode(raw(FrameKind::Item, vec![0xff; 12])).is_none());
      let ok = encode_exit(&WireExit::Success {
        value: Postcard::encode(&3_i64).unwrap(),
      })
      .unwrap();
      assert_eq!(
        len.decode(raw(FrameKind::Exit, ok)).unwrap().payload,
        Some(json!({"_tag": "Success", "value": 3}))
      );
      let failed = encode_exit(&WireExit::Failure {
        error: json!({"_tag": "Cancelled"}),
      })
      .unwrap();
      assert_eq!(
        len.decode(raw(FrameKind::Exit, failed)).unwrap().payload,
        Some(json!({"_tag": "Failure", "error": {"_tag": "Cancelled"}}))
      );
      assert!(len.decode(raw(FrameKind::Exit, vec![9])).is_none());
      // A payload on a kind that carries none is dropped.
      assert_eq!(
        len.decode(raw(FrameKind::Credit, vec![1])).unwrap().payload,
        None
      );
      // A stream's successful exit is `null`.
      let up = postcard_wire::Bridges::of(&group, "T.up", None).unwrap();
      let unit = encode_exit(&WireExit::Success { value: Vec::new() }).unwrap();
      assert_eq!(
        up.decode(raw(FrameKind::Exit, unit)).unwrap().payload,
        Some(json!({"_tag": "Success", "value": null}))
      );
    }

    #[tokio::test]
    async fn reader_skips_stale_frames_and_closes_on_garbage() {
      let (client_io, server_io) = tokio::io::duplex(1 << 16);
      let (client_ws, mut server_ws) = tokio::join!(
        WebSocketStream::from_raw_socket(client_io, Role::Client, None),
        WebSocketStream::from_raw_socket(server_io, Role::Server, None),
      );
      let hello = Hello {
        session: SessionId::random(),
        codec: "idrpc.postcard.v1".to_owned(),
        max_frame: 65_536,
        window: 4,
      };
      let greeting = server_frame(
        CallId::CONNECTION,
        FrameKind::Hello,
        Postcard::encode(&hello).unwrap(),
      );
      server_ws.send(greeting).await.unwrap();
      let client = WsClient::over_postcard(client_ws, group()).await.unwrap();
      assert_eq!(
        (client.codec(), client.hello().window),
        (CodecId::Postcard, 4)
      );

      // A frame of a call the client does not have is skipped; the connection goes on.
      server_ws
        .send(server_frame(id(99), FrameKind::Item, vec![1]))
        .await
        .unwrap();
      let call = client.open("T.len", json!("ab"), None).await.unwrap();
      let opened = decode_frame(&binary(server_ws.next().await.unwrap().unwrap())).unwrap();
      let exit = encode_exit(&WireExit::Success {
        value: Postcard::encode(&2_i64).unwrap(),
      })
      .unwrap();
      server_ws
        .send(server_frame(opened.id, FrameKind::Exit, exit))
        .await
        .unwrap();
      assert_eq!(call.finish().await.unwrap(), json!(2));

      // Bytes that are no frame close the connection: open calls end, new ones fail.
      let mut pending = client.open("T.up", json!(1), None).await.unwrap();
      let _call_frame = server_ws.next().await;
      server_ws
        .send(Message::Binary(vec![0xff, 0xff].into()))
        .await
        .unwrap();
      assert!(matches!(
        pending.next().await,
        Some(CallEvent::Exit(Err(_)))
      ));
      assert!(client.open("T.len", json!("x"), None).await.is_err());
    }
  }
}
