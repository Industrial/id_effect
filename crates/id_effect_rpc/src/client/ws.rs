//! The WebSocket client (feature `ws`): one socket, every procedure kind multiplexed on it by
//! call id, with the server's credit backpressure honoured both ways (design note §5).
//!
//! [`WsClient::connect`] offers `idrpc.json.v1`, reads the server's `hello`, then routes frames:
//! each [`WsCall`] gets its own items and exit. A call grants the server credits back as its
//! items are read (half a window at a time), so an unread stream stops its producer on the server;
//! [`WsCall::send`] waits for credits the server granted. Dropping a [`WsCall`] before its exit
//! cancels the call; dropping the last handle to the socket closes it.
//!
//! `connect` speaks plain `ws://`; for `wss://` hand [`WsClient::over`] a socket opened with
//! `tokio_tungstenite::client_async` over a TLS stream of your choice.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;

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

fn message(frame: &Frame) -> Result<Message, RpcClientError> {
  let bytes = frame::encode(frame, CodecId::Json).map_err(transport)?;
  Ok(Message::Text(
    String::from_utf8_lossy(&bytes).into_owned().into(),
  ))
}

/// One open call's mailbox.
struct Slot {
  frames: mpsc::Sender<Frame>,
  credits: Arc<Semaphore>,
}

struct Shared {
  out: mpsc::Sender<Message>,
  calls: Mutex<HashMap<CallId, Slot>>,
  next_id: AtomicU64,
  closed: AtomicBool,
  hello: Hello,
  tasks: [JoinHandle<()>; 2],
}

impl Shared {
  fn calls(&self) -> std::sync::MutexGuard<'_, HashMap<CallId, Slot>> {
    self.calls.lock().unwrap_or_else(PoisonError::into_inner)
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
  /// Connect to `url` (`ws://host/rpc/ws`, see
  /// [`RpcClientConfig::ws_url`](super::RpcClientConfig::ws_url)) and read the `hello`.
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`]: the handshake failed or the server refused it (its status),
  /// or the first frame was not a `hello`.
  pub async fn connect(url: &str) -> Result<Self, RpcClientError> {
    let mut request = url.into_client_request().map_err(transport)?;
    request.headers_mut().insert(
      "sec-websocket-protocol",
      HeaderValue::from_static(CodecId::Json.subprotocol()),
    );
    let (socket, _response) = tokio_tungstenite::connect_async(request)
      .await
      .map_err(transport)?;
    Self::over(socket).await
  }

  /// Run the RPC protocol on an open socket (one whose handshake offered `idrpc.json.v1`).
  ///
  /// # Errors
  ///
  /// [`RpcClientError::Transport`] when the first frame is not a `hello`.
  pub async fn over<S>(socket: WebSocketStream<S>) -> Result<Self, RpcClientError>
  where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
  {
    let (mut sink, mut stream) = socket.split();
    let hello = match stream.next().await {
      Some(Ok(Message::Text(text))) => frame::decode(text.as_bytes(), CodecId::Json)
        .ok()
        .filter(|f| f.kind == FrameKind::Hello)
        .and_then(|f| serde_json::from_value::<Hello>(f.payload?).ok()),
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
        let Ok(frame) = frame::decode(bytes, CodecId::Json) else {
          break;
        };
        let Some(shared) = weak.upgrade() else { return };
        if !route(&shared, frame) {
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
      tasks: [reader, writer],
    });
    let _ = ready_tx.send(Arc::clone(&shared));
    Ok(Self { shared })
  }

  /// The server's `hello`: session id, codec, frame cap, credit window.
  pub fn hello(&self) -> &Hello {
    &self.shared.hello
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
  /// [`RpcClientError::Transport`] once the socket is closed.
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
    let n = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let id = CallId::new(n).map_err(transport)?;
    let window = shared.hello.window as usize;
    let (tx, rx) = mpsc::channel(window + 1);
    let credits = Arc::new(Semaphore::new(window));
    shared.calls().insert(
      id,
      Slot {
        frames: tx,
        credits: Arc::clone(&credits),
      },
    );
    let mut call = Frame::call(id, tag, payload);
    call.cursor = after;
    call.v = v;
    let call_msg = message(&call)?;
    let call = WsCall {
      id,
      frames: rx,
      credits,
      shared: Arc::clone(shared),
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
}

impl ItemSink {
  async fn send_frame(&self, frame: &Frame) -> Result<(), RpcClientError> {
    let msg = message(frame)?;
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
  TransportError::Io(err.to_string())
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
      && let Ok(msg) = message(&Frame::new(self.id, FrameKind::Cancel))
    {
      let _ = self.shared.out.try_send(msg);
    }
  }
}
