//! Calls to a service's mesh endpoint ([`crate::transport::mesh`]; features `client` and
//! `mesh`), as effects, postcard always.
//!
//! [`MeshRpcClient`] resolves the endpoint's cluster-wide name on every call, so an endpoint
//! that restarted under its supervisor (a new process, the same name) is found again. A query
//! whose endpoint is gone (no such process, it exited mid-call, its node is down) is re-resolved
//! and retried a bounded number of times ([`MeshRpcClient::with_query_retries`]); a mutation is
//! never retried: it fails with [`MeshRpcError::NoEndpoint`] / [`MeshRpcError::EndpointDown`] /
//! [`MeshRpcError::NodeDown`] and the caller decides.
//!
//! Every call carries the calling fiber's span as its trace context (the remote `rpc.request`
//! span is its child) and, for queries, mutations and client streams, the client's timeout as
//! its deadline ([`MeshRpcClient::with_timeout`], default 30 s; the endpoint clamps it). A call
//! never hangs on a dead endpoint: it monitors the endpoint process, and the monitor fires when
//! the process exits or its node goes down. A stream is also watched by heartbeats: when the
//! endpoint's node stays silent past the deadline ([`MeshLiveness`]), the stream fails with
//! [`MeshRpcError::PeerSilent`] even if nothing reported the node down. Dropping a call (or its
//! stream) cancels it.
//!
//! [`MeshLiveness`]: crate::transport::mesh::MeshLiveness
//!
//! A `#[rpc::service]` client calls over the mesh through [`MeshTransport`].
//!
//! ```rust,no_run
//! # async fn demo(rpc: id_effect_rpc::transport::mesh::MeshRpc) {
//! use id_effect::Effect;
//! use id_effect_rpc::client::mesh::MeshRpcError;
//!
//! let greeter = rpc.client("Greeter");
//! let hello: Effect<String, MeshRpcError, ()> = greeter.query("Greeter.greet", &"Ada".to_owned());
//! # let _ = hello;
//! # }
//! ```

use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use id_effect::{Effect, Span, fail, from_async};
use id_effect_node::{RemoteFault, TraceContext};
use id_effect_process::ExitReason;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::codec::postcard::{Postcard, decode_exit};
use crate::codec::{Codec as _, CodecId, TypedCodec};
use crate::contract::ProcedureKind;
use crate::registry::RpcGroup;
use crate::server::dispatch;
use crate::service::{
  BoxFuture, FrameStream, ItemReceiver, RpcTransport, TransportCall, TransportError,
};
use crate::transport::mesh::{
  CallShared, ClientEvent, MeshRpc, Node, OpenCall, REJECTED_TAG, endpoint_name,
};
use crate::wire::{CallId, Cursor, Frame, FrameKind, INITIAL_CREDITS, RpcFailure, WireExit};

/// Default timeout of queries, mutations and client streams (also their deadline).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default number of times a query is re-resolved and retried when its endpoint is gone.
pub const DEFAULT_QUERY_RETRIES: u32 = 3;

/// Default pause before the first retry (it grows linearly).
pub const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_millis(50);

const CREDIT_BATCH: u32 = INITIAL_CREDITS / 2;

/// The event of a call whose reply slot is gone.
const CLOSED: &str = "the call closed";

/// Why a mesh call failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum MeshRpcError {
  /// The procedure's failure: its typed error (`{"_tag": …}`), a reserved failure
  /// ([`MeshRpcError::reserved`]: `DeadlineExceeded` also when the client's timeout passed), or a
  /// refusal before it ran ([`MeshRpcError::rejected_status`]).
  #[error("rpc failure {}", failure_tag(.0))]
  Failure(Value),
  /// No live endpoint holds the service's name.
  #[error("no mesh endpoint for service {0}")]
  NoEndpoint(String),
  /// The endpoint process exited during the call.
  #[error("the mesh endpoint exited: {0}")]
  EndpointDown(String),
  /// The endpoint's node is unreachable.
  #[error("the mesh endpoint's node is unreachable")]
  NodeDown,
  /// Nothing was heard from the endpoint's node for longer than the liveness deadline
  /// ([`MeshLiveness`](crate::transport::mesh::MeshLiveness)): the stream failed and was
  /// cancelled, although nothing reported the node down.
  #[error("the mesh peer {node} was silent for {silent_for:?}")]
  PeerSilent {
    /// The endpoint's node.
    node: String,
    /// How long it had been silent.
    silent_for: Duration,
  },
  /// An input or output did not encode or decode.
  #[error("codec: {0}")]
  Codec(String),
  /// The call broke the protocol, or the mesh refused a send.
  #[error("transport: {0}")]
  Transport(String),
}

fn failure_tag(error: &Value) -> &str {
  error
    .get("_tag")
    .and_then(Value::as_str)
    .unwrap_or("(untagged)")
}

impl MeshRpcError {
  /// The `_tag` of a [`MeshRpcError::Failure`].
  pub fn failure_tag(&self) -> Option<&str> {
    match self {
      Self::Failure(error) => Some(failure_tag(error)),
      _ => None,
    }
  }

  /// The reserved failure of a [`MeshRpcError::Failure`], if it is one.
  pub fn reserved(&self) -> Option<RpcFailure> {
    match self {
      Self::Failure(error) if RpcFailure::is_reserved(failure_tag(error)) => {
        serde_json::from_value(error.clone()).ok()
      }
      _ => None,
    }
  }

  /// The status of a refusal before the call ran (`Rejected`: 404 unknown procedure, 401/403 a
  /// guard, 415 a procedure without a typed codec).
  pub fn rejected_status(&self) -> Option<u16> {
    match self {
      Self::Failure(error) if failure_tag(error) == REJECTED_TAG => error
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok()),
      _ => None,
    }
  }

  /// Whether the endpoint was gone (what a query retries on).
  pub fn is_endpoint_gone(&self) -> bool {
    matches!(
      self,
      Self::NoEndpoint(_) | Self::EndpointDown(_) | Self::NodeDown | Self::PeerSilent { .. }
    )
  }
}

fn codec_error(e: impl std::fmt::Display) -> MeshRpcError {
  MeshRpcError::Codec(e.to_string())
}

/// A send the mesh refused: no route to the endpoint's node, or (on an in-process link) the
/// endpoint process is gone — either way the endpoint is not there.
fn send_error(service: &str, fault: RemoteFault) -> MeshRpcError {
  match fault {
    RemoteFault::NoProc => MeshRpcError::NoEndpoint(service.to_owned()),
    other => MeshRpcError::Transport(other.to_string()),
  }
}

fn down_error(service: &str, reason: &ExitReason) -> MeshRpcError {
  match reason {
    ExitReason::NoProc => MeshRpcError::NoEndpoint(service.to_owned()),
    ExitReason::NoConnection => MeshRpcError::NodeDown,
    other => MeshRpcError::EndpointDown(format!("{other:?}")),
  }
}

/// The error of a client event that ends a call (anything but a frame).
fn event_error(service: &str, event: ClientEvent) -> MeshRpcError {
  match event {
    ClientEvent::Down(reason) => down_error(service, &reason),
    ClientEvent::Silent { node, silent_for } => MeshRpcError::PeerSilent { node, silent_for },
    ClientEvent::Violation(why) => MeshRpcError::Transport(why.into()),
    ClientEvent::Frame(_) => MeshRpcError::Transport("an unexpected frame".into()),
  }
}

fn deadline_exceeded() -> MeshRpcError {
  MeshRpcError::Failure(json!({ "_tag": "DeadlineExceeded" }))
}

/// The value of an `exit` frame's payload.
fn exit_value(payload: Option<&[u8]>) -> Result<Vec<u8>, MeshRpcError> {
  let bytes = payload.ok_or_else(|| MeshRpcError::Transport("an exit without a payload".into()))?;
  match decode_exit(bytes).map_err(codec_error)? {
    WireExit::Success { value } => Ok(value),
    WireExit::Failure { error } => Err(MeshRpcError::Failure(error)),
  }
}

impl MeshRpc {
  /// A client of `service`'s endpoint, wherever it runs.
  pub fn client(&self, service: impl Into<String>) -> MeshRpcClient {
    MeshRpcClient {
      rpc: self.clone(),
      service: Arc::from(service.into()),
      timeout: DEFAULT_TIMEOUT,
      retries: DEFAULT_QUERY_RETRIES,
      backoff: DEFAULT_RETRY_BACKOFF,
    }
  }
}

/// Calls the procedures of one service over the mesh ([`MeshRpc::client`]).
#[derive(Clone, Debug)]
pub struct MeshRpcClient {
  rpc: MeshRpc,
  service: Arc<str>,
  timeout: Duration,
  retries: u32,
  backoff: Duration,
}

impl MeshRpcClient {
  /// Give up on queries, mutations and client streams after `timeout` (sent as their deadline).
  #[must_use]
  pub fn with_timeout(mut self, timeout: Duration) -> Self {
    self.timeout = timeout;
    self
  }

  /// Retry a query at most `retries` times when its endpoint is gone (0: never).
  #[must_use]
  pub fn with_query_retries(mut self, retries: u32) -> Self {
    self.retries = retries;
    self
  }

  /// Pause `backoff × attempt` before each query retry.
  #[must_use]
  pub fn with_retry_backoff(mut self, backoff: Duration) -> Self {
    self.backoff = backoff;
    self
  }

  /// The service.
  pub fn service(&self) -> &str {
    &self.service
  }

  /// Run `body` with the calling fiber's span as trace context.
  fn traced<A, R, F, Fut>(&self, body: F) -> Effect<A, MeshRpcError, R>
  where
    A: Send + 'static,
    R: Send + Sync + 'static,
    F: FnOnce(TraceContext) -> Fut + Send + 'static,
    Fut: Future<Output = Result<A, MeshRpcError>> + Send + 'static,
  {
    let this = self.clone();
    Span::current().flat_map(move |span| {
      let trace = span.map_or_else(TraceContext::new, |span| this.span_context(&span));
      from_async(move || body(trace))
    })
  }

  /// `span`'s context as the node's propagator writes it.
  fn span_context(&self, span: &Span) -> TraceContext {
    let mut trace = TraceContext::new();
    self.rpc.node.propagator().inject(span, &mut trace);
    trace
  }

  /// Resolve the endpoint, open a call to it and send the `call` frame.
  #[allow(clippy::too_many_arguments)]
  fn open(
    &self,
    tag: &str,
    version: Option<u32>,
    payload: Option<Vec<u8>>,
    after: Option<Cursor>,
    trace: TraceContext,
    deadline: Option<Duration>,
    items: bool,
  ) -> Result<OpenCall, MeshRpcError> {
    let endpoint = self
      .rpc
      .mesh()
      .global()
      .whereis(&endpoint_name(&self.service))
      .ok_or_else(|| MeshRpcError::NoEndpoint(self.service.to_string()))?;
    let call = self
      .rpc
      .node
      .open_call(endpoint, items)
      .map_err(|fault| send_error(&self.service, fault))?;
    let mut frame: Frame<Vec<u8>> = Frame::new(call.shared.id(), FrameKind::Call);
    frame.tag = Some(tag.to_owned());
    frame.v = version;
    frame.cursor = after;
    frame.payload = payload;
    call
      .shared
      .send(&frame, trace, deadline)
      .map_err(|fault| send_error(&self.service, fault))?;
    Ok(call)
  }

  /// A query (`retry`: re-resolved and retried on a gone endpoint) or a mutation: the output
  /// bytes.
  async fn unary(
    &self,
    tag: &str,
    version: Option<u32>,
    payload: Vec<u8>,
    trace: TraceContext,
    retry: bool,
  ) -> Result<Vec<u8>, MeshRpcError> {
    let retries = if retry { self.retries } else { 0 };
    let mut attempt = 0;
    loop {
      let once = async {
        let call = self.open(
          tag,
          version,
          Some(payload.clone()),
          None,
          trace.clone(),
          Some(self.timeout),
          false,
        )?;
        call.result(&self.service, self.timeout).await
      };
      match once.await {
        Err(e) if e.is_endpoint_gone() && attempt < retries => {
          attempt += 1;
          tokio::time::sleep(self.backoff * attempt).await;
        }
        other => return other,
      }
    }
  }

  /// Call the query `tag` with `input`: retried on a gone endpoint (re-resolved each time).
  pub fn query<I, O, R>(&self, tag: &str, input: &I) -> Effect<O, MeshRpcError, R>
  where
    I: Serialize + ?Sized,
    O: DeserializeOwned + Send + 'static,
    R: Send + Sync + 'static,
  {
    let payload = match Postcard::encode(input) {
      Ok(payload) => payload,
      Err(e) => return fail(codec_error(e)),
    };
    let (this, tag) = (self.clone(), tag.to_owned());
    self.traced(move |trace| async move {
      let bytes = this.unary(&tag, None, payload, trace, true).await?;
      Postcard::decode(&bytes).map_err(codec_error)
    })
  }

  /// Call the mutation `tag` with `input` (never retried).
  pub fn mutation<I, O, R>(&self, tag: &str, input: &I) -> Effect<O, MeshRpcError, R>
  where
    I: Serialize + ?Sized,
    O: DeserializeOwned + Send + 'static,
    R: Send + Sync + 'static,
  {
    let payload = match Postcard::encode(input) {
      Ok(payload) => payload,
      Err(e) => return fail(codec_error(e)),
    };
    let (this, tag) = (self.clone(), tag.to_owned());
    self.traced(move |trace| async move {
      let bytes = this.unary(&tag, None, payload, trace, false).await?;
      Postcard::decode(&bytes).map_err(codec_error)
    })
  }

  /// Open the server stream `tag` with `input`.
  pub fn server_stream<I, O, R>(
    &self,
    tag: &str,
    input: &I,
  ) -> Effect<MeshStream<O>, MeshRpcError, R>
  where
    I: Serialize + ?Sized,
    O: DeserializeOwned + Send + 'static,
    R: Send + Sync + 'static,
  {
    self.subscribe(tag, input, None)
  }

  /// Open the subscription `tag` with `input`, resuming after `after`
  /// ([`MeshStream::cursor`] is the position of the last item read).
  pub fn subscribe<I, O, R>(
    &self,
    tag: &str,
    input: &I,
    after: Option<Cursor>,
  ) -> Effect<MeshStream<O>, MeshRpcError, R>
  where
    I: Serialize + ?Sized,
    O: DeserializeOwned + Send + 'static,
    R: Send + Sync + 'static,
  {
    let payload = match Postcard::encode(input) {
      Ok(payload) => payload,
      Err(e) => return fail(codec_error(e)),
    };
    let (this, tag) = (self.clone(), tag.to_owned());
    self.traced(move |trace| async move {
      let call = this.open(&tag, None, Some(payload), after, trace, None, true)?;
      Ok(MeshStream::new(call, Arc::clone(&this.service)))
    })
  }

  /// Open the client stream `tag`: send items through the sink ([`MeshSink::close`] or drop it
  /// to end the input), then read the output from the reply.
  pub fn client_stream<I, O, R>(
    &self,
    tag: &str,
  ) -> Effect<(MeshSink<I>, MeshReply<O>), MeshRpcError, R>
  where
    I: Serialize + Send + 'static,
    O: DeserializeOwned + Send + 'static,
    R: Send + Sync + 'static,
  {
    let (this, tag) = (self.clone(), tag.to_owned());
    self.traced(move |trace| async move {
      let call = this.open(&tag, None, None, None, trace, Some(this.timeout), false)?;
      let sink = MeshSink::new(Arc::clone(&call.shared));
      Ok((
        sink,
        MeshReply {
          call,
          service: Arc::clone(&this.service),
          timeout: this.timeout,
          _output: PhantomData,
        },
      ))
    })
  }

  /// Open the two-way stream `tag`: items go out through the sink, the server's come in on the
  /// stream, both under credits.
  pub fn bidi<I, O, R>(&self, tag: &str) -> Effect<(MeshSink<I>, MeshStream<O>), MeshRpcError, R>
  where
    I: Serialize + Send + 'static,
    O: DeserializeOwned + Send + 'static,
    R: Send + Sync + 'static,
  {
    let (this, tag) = (self.clone(), tag.to_owned());
    self.traced(move |trace| async move {
      let call = this.open(&tag, None, None, None, trace, None, true)?;
      let sink = MeshSink::new(Arc::clone(&call.shared));
      Ok((sink, MeshStream::new(call, Arc::clone(&this.service))))
    })
  }
}

impl OpenCall {
  /// The call's output bytes, within `timeout`.
  async fn result(mut self, service: &str, timeout: Duration) -> Result<Vec<u8>, MeshRpcError> {
    match tokio::time::timeout(timeout, self.exit(service)).await {
      Ok(result) => result,
      Err(_) => Err(deadline_exceeded()),
    }
  }

  async fn exit(&mut self, service: &str) -> Result<Vec<u8>, MeshRpcError> {
    let event = self
      .rx
      .recv()
      .await
      .unwrap_or(ClientEvent::Violation(CLOSED));
    let outcome = match event {
      ClientEvent::Frame(frame) if frame.kind == FrameKind::Exit => {
        exit_value(frame.payload.as_deref())
      }
      other => Err(event_error(service, other)),
    };
    self.shared.finish();
    outcome
  }
}

/// The items of a server stream, subscription or two-way stream: a [`futures_util::Stream`] of
/// `Result<O, MeshRpcError>`, ending after the exit (a failed exit is its last item). Reading
/// grants the endpoint credits; dropping the stream cancels the call.
pub struct MeshStream<O> {
  raw: RawStream,
  _item: PhantomData<fn() -> O>,
}

impl<O> std::fmt::Debug for MeshStream<O> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("MeshStream")
      .field("id", &self.raw.call.shared.id())
      .field("done", &self.raw.done)
      .finish_non_exhaustive()
  }
}

impl<O: DeserializeOwned> MeshStream<O> {
  fn new(call: OpenCall, service: Arc<str>) -> Self {
    Self {
      raw: RawStream::new(call, service),
      _item: PhantomData,
    }
  }

  /// The cursor of the last item read (subscriptions resume after it).
  pub fn cursor(&self) -> Option<Cursor> {
    self.raw.cursor
  }

  /// The next item; `None` after the exit.
  pub async fn recv(&mut self) -> Option<Result<O, MeshRpcError>> {
    futures_util::StreamExt::next(self).await
  }
}

impl<O: DeserializeOwned> futures_util::Stream for MeshStream<O> {
  type Item = Result<O, MeshRpcError>;

  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    futures_util::Stream::poll_next(Pin::new(&mut self.get_mut().raw), cx).map(|next| {
      next.map(|item| item.and_then(|(_, bytes)| Postcard::decode(&bytes).map_err(codec_error)))
    })
  }
}

/// The items of a stream call as postcard bytes with their cursors.
struct RawStream {
  call: OpenCall,
  service: Arc<str>,
  consumed: u32,
  cursor: Option<Cursor>,
  done: bool,
}

type RawItem = Result<(Option<Cursor>, Vec<u8>), MeshRpcError>;

impl RawStream {
  fn new(call: OpenCall, service: Arc<str>) -> Self {
    Self {
      call,
      service,
      consumed: 0,
      cursor: None,
      done: false,
    }
  }

  fn over(&mut self, last: Option<RawItem>) -> Poll<Option<RawItem>> {
    self.done = true;
    self.call.shared.finish();
    Poll::Ready(last)
  }
}

impl futures_util::Stream for RawStream {
  type Item = RawItem;

  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    let this = self.get_mut();
    if this.done {
      return Poll::Ready(None);
    }
    loop {
      let event = match this.call.rx.poll_recv(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(event) => event.unwrap_or(ClientEvent::Violation(CLOSED)),
      };
      match event {
        ClientEvent::Frame(frame) => match frame.kind {
          FrameKind::Item => {
            this.consumed += 1;
            if this.consumed >= CREDIT_BATCH {
              this.call.shared.grant(this.consumed);
              this.consumed = 0;
            }
            this.cursor = frame.cursor.or(this.cursor);
            return Poll::Ready(Some(Ok((frame.cursor, frame.payload.unwrap_or_default()))));
          }
          FrameKind::Exit => {
            let last = exit_value(frame.payload.as_deref()).err().map(Err);
            return this.over(last);
          }
          _ => {}
        },
        ended => {
          let error = event_error(&this.service, ended);
          return this.over(Some(Err(error)));
        }
      }
    }
  }
}

/// The output of a client stream ([`MeshRpcClient::client_stream`]).
pub struct MeshReply<O> {
  call: OpenCall,
  service: Arc<str>,
  timeout: Duration,
  _output: PhantomData<fn() -> O>,
}

impl<O> std::fmt::Debug for MeshReply<O> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("MeshReply")
      .field("id", &self.call.shared.id())
      .finish_non_exhaustive()
  }
}

impl<O: DeserializeOwned> MeshReply<O> {
  /// Wait for the output (within the client's timeout, counted from here).
  pub async fn recv(self) -> Result<O, MeshRpcError> {
    let Self {
      call,
      service,
      timeout,
      ..
    } = self;
    let bytes = call.result(&service, timeout).await?;
    Postcard::decode(&bytes).map_err(codec_error)
  }
}

/// The sending half of a client stream or two-way stream: one item per credit the endpoint
/// granted (initially [`INITIAL_CREDITS`]). Dropping it ends the input.
pub struct MeshSink<I> {
  shared: Arc<CallShared>,
  _item: PhantomData<fn(I)>,
}

impl<I> std::fmt::Debug for MeshSink<I> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("MeshSink")
      .field("id", &self.shared.id())
      .finish_non_exhaustive()
  }
}

impl<I: Serialize> MeshSink<I> {
  fn new(shared: Arc<CallShared>) -> Self {
    Self {
      shared,
      _item: PhantomData,
    }
  }

  /// Send `item`, waiting for a credit.
  ///
  /// # Errors
  ///
  /// [`MeshRpcError::Codec`] when it does not encode; [`MeshRpcError::Transport`] when the
  /// call is over or the mesh refused the send.
  pub async fn send(&self, item: &I) -> Result<(), MeshRpcError> {
    let bytes = Postcard::encode(item).map_err(codec_error)?;
    if !self.shared.take_credit().await {
      return Err(MeshRpcError::Transport("the call is over".into()));
    }
    self
      .shared
      .send(
        &Frame::new(self.shared.id(), FrameKind::Item).with_payload(bytes),
        TraceContext::new(),
        None,
      )
      .map_err(|fault| MeshRpcError::Transport(fault.to_string()))
  }

  /// End the input (idempotent).
  ///
  /// # Errors
  ///
  /// [`MeshRpcError::Transport`] when the mesh refused the send.
  pub fn close(&self) -> Result<(), MeshRpcError> {
    self
      .shared
      .end()
      .map_err(|fault| MeshRpcError::Transport(fault.to_string()))
  }
}

impl<I> Drop for MeshSink<I> {
  fn drop(&mut self) {
    let _ = self.shared.end();
  }
}

// ---------------------------------------------------------------------------------------------
// `#[rpc::service]` clients
// ---------------------------------------------------------------------------------------------

/// The [`RpcTransport`] of a `#[rpc::service]` client over the mesh:
/// `GreeterService::client(MeshTransport::new(rpc.client("Greeter"), GreeterService::group(..)))`.
///
/// A generated client hands its transport JSON values; postcard, which the mesh always speaks,
/// is not self-describing, so the transport encodes inputs and decodes outputs with the typed
/// codecs (`WireSchema::with_serde`) of the service's procedures, taken from `schemas`: the
/// service's own [`RpcGroup`] (its handlers never run here). A procedure `schemas` lacks is
/// refused (404) and one without typed codecs too (415), before anything is sent. Queries are
/// retried on a gone endpoint like [`MeshRpcClient::query`]. Every kind is carried.
///
/// Trace context crosses as on the 0.11 mesh sends (dist.remote#8): a generated client's queries
/// and mutations run as effects of the calling fiber, so each carries that fiber's span and the
/// remote `rpc.request` span is its child. The stream kinds are opened outside any fiber; they
/// carry the context the transport was given ([`MeshTransport::in_current_span`],
/// [`MeshTransport::with_trace_context`]), none by default.
#[derive(Clone)]
pub struct MeshTransport {
  client: MeshRpcClient,
  schemas: Arc<RpcGroup>,
  trace: TraceContext,
}

impl std::fmt::Debug for MeshTransport {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("MeshTransport")
      .field("service", &self.client.service)
      .field("procedures", &self.schemas.len())
      .finish_non_exhaustive()
  }
}

/// The typed codecs of one procedure.
struct Bridges {
  input: TypedCodec,
  output: TypedCodec,
}

fn refused(status: u16) -> TransportError {
  TransportError::Refused {
    status,
    problem: None,
  }
}

fn transport_error(error: MeshRpcError) -> TransportError {
  match error {
    MeshRpcError::Codec(why) => TransportError::Protocol(why),
    other => TransportError::Io(other.to_string()),
  }
}

/// A call's failure as the exit a generated client reads: a refusal before it ran is
/// [`TransportError::Refused`], as over HTTP.
fn failure_exit(error: MeshRpcError) -> Result<WireExit, TransportError> {
  if let Some(status) = error.rejected_status() {
    return Err(refused(status));
  }
  match error {
    MeshRpcError::Failure(error) => Ok(WireExit::Failure { error }),
    other => Err(transport_error(other)),
  }
}

fn exit_json(id: CallId, exit: &WireExit) -> Frame {
  // A tagged union of JSON values always serializes.
  Frame::new(id, FrameKind::Exit).with_payload(serde_json::to_value(exit).unwrap_or_default())
}

impl Bridges {
  fn input(&self, value: &Value) -> Result<Vec<u8>, TransportError> {
    self
      .input
      .encode(CodecId::Postcard, value)
      .map_err(|e| TransportError::Protocol(e.to_string()))
  }

  fn exit(&self, result: Result<Vec<u8>, MeshRpcError>) -> Result<WireExit, TransportError> {
    match result {
      Ok(bytes) => Ok(WireExit::Success {
        value: self
          .output
          .decode(CodecId::Postcard, &bytes)
          .map_err(|e| TransportError::Protocol(e.to_string()))?,
      }),
      Err(error) => failure_exit(error),
    }
  }

  /// The JSON frames of a stream call: items, then one exit; a transport failure ends the
  /// frames without an exit.
  fn frames(&self, raw: RawStream) -> FrameStream {
    let (id, output) = (raw.call.shared.id(), self.output.clone());
    Box::pin(futures_util::stream::unfold(Some(raw), move |state| {
      let output = output.clone();
      async move {
        let mut raw = state?;
        match futures_util::StreamExt::next(&mut raw).await {
          Some(Ok((cursor, bytes))) => {
            let value = output.decode(CodecId::Postcard, &bytes).ok()?;
            let mut frame = Frame::new(id, FrameKind::Item).with_payload(value);
            frame.cursor = cursor;
            Some((frame, Some(raw)))
          }
          Some(Err(error)) => {
            let exit = failure_exit(error).ok()?;
            Some((exit_json(id, &exit), None))
          }
          None => {
            let exit = WireExit::Success { value: Value::Null };
            Some((exit_json(id, &exit), None))
          }
        }
      }
    }))
  }

  /// Send a client's items under the endpoint's credits, then end the input.
  fn feed(&self, node: &Node, shared: Arc<CallShared>, mut items: ItemReceiver) {
    let input = self.input.clone();
    node.spawn(async move {
      while let Some(value) = items.recv().await {
        let Ok(bytes) = input.encode(CodecId::Postcard, &value) else {
          break;
        };
        let frame = Frame::new(shared.id(), FrameKind::Item).with_payload(bytes);
        if !shared.take_credit().await || shared.send(&frame, TraceContext::new(), None).is_err() {
          break;
        }
      }
      let _ = shared.end();
    });
  }
}

impl MeshTransport {
  /// Call through `client`, with the codecs of `schemas` (the service's group).
  pub fn new(client: MeshRpcClient, schemas: RpcGroup) -> Self {
    Self {
      client,
      schemas: Arc::new(schemas),
      trace: TraceContext::new(),
    }
  }

  /// Carry `trace` on every call opened outside a fiber (the stream kinds, and
  /// [`RpcTransport::unary`] awaited directly).
  #[must_use]
  pub fn with_trace_context(mut self, trace: TraceContext) -> Self {
    self.trace = trace;
    self
  }

  /// The trace context carried on calls opened outside a fiber.
  pub fn trace_context(&self) -> &TraceContext {
    &self.trace
  }

  /// This transport carrying the calling fiber's current span (through the node's
  /// [`TracePropagator`](id_effect_node::TracePropagator)) on every call, the stream kinds
  /// included; outside any span, the transport as it is.
  pub fn in_current_span<E, R>(&self) -> Effect<Self, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let this = self.clone();
    Span::current().map(move |span| match span {
      Some(span) => {
        let trace = this.client.span_context(&span);
        this.with_trace_context(trace)
      }
      None => this,
    })
  }

  fn bridges(&self, call: &TransportCall) -> Result<Bridges, TransportError> {
    let entry =
      dispatch::resolve(&self.schemas, &call.tag, Some(call.version)).map_err(|_| refused(404))?;
    let codecs = entry.codecs();
    match (codecs.input(), codecs.output()) {
      (Some(input), Some(output)) => Ok(Bridges {
        input: input.clone(),
        output: output.clone(),
      }),
      _ => Err(refused(415)),
    }
  }

  fn open(
    &self,
    call: &TransportCall,
    payload: Option<Vec<u8>>,
    after: Option<Cursor>,
    deadline: Option<Duration>,
    items: bool,
  ) -> Result<OpenCall, TransportError> {
    self
      .client
      .open(
        &call.tag,
        Some(call.version),
        payload,
        after,
        self.trace.clone(),
        deadline,
        items,
      )
      .map_err(transport_error)
  }

  /// A query or mutation carrying `trace`.
  async fn unary_traced(
    &self,
    call: TransportCall,
    trace: TraceContext,
  ) -> Result<WireExit, TransportError> {
    let bridges = self.bridges(&call)?;
    let payload = bridges.input(&call.payload)?;
    let retry = call.kind == ProcedureKind::Query;
    let result = self
      .client
      .unary(&call.tag, Some(call.version), payload, trace, retry)
      .await;
    bridges.exit(result)
  }
}

impl RpcTransport for MeshTransport {
  fn unary(&self, call: TransportCall) -> BoxFuture<Result<WireExit, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let trace = this.trace.clone();
      this.unary_traced(call, trace).await
    })
  }

  /// The generated client's queries and mutations: the calling fiber's span is the call's trace
  /// context (else the transport's own, [`MeshTransport::with_trace_context`]).
  fn unary_effect<R: Send + Sync + 'static>(
    &self,
    call: TransportCall,
  ) -> Effect<WireExit, TransportError, R> {
    let this = self.clone();
    Span::current().flat_map(move |span| {
      let trace = span.map_or_else(
        || this.trace.clone(),
        |span| this.client.span_context(&span),
      );
      from_async(move || async move { this.unary_traced(call, trace).await })
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
      let bridges = this.bridges(&call)?;
      let open = this.open(&call, None, None, Some(this.client.timeout), false)?;
      bridges.feed(&this.client.rpc.node, Arc::clone(&open.shared), items);
      let result = open.result(&this.client.service, this.client.timeout).await;
      bridges.exit(result)
    })
  }

  fn bidi(
    &self,
    call: TransportCall,
    items: ItemReceiver,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let bridges = this.bridges(&call)?;
      let open = this.open(&call, None, None, None, true)?;
      bridges.feed(&this.client.rpc.node, Arc::clone(&open.shared), items);
      Ok(bridges.frames(RawStream::new(open, Arc::clone(&this.client.service))))
    })
  }

  fn subscribe(
    &self,
    call: TransportCall,
    after: Option<Cursor>,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let bridges = this.bridges(&call)?;
      let payload = bridges.input(&call.payload)?;
      let open = this.open(&call, Some(payload), after, None, true)?;
      Ok(bridges.frames(RawStream::new(open, Arc::clone(&this.client.service))))
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_refused_send_is_a_missing_endpoint() {
    assert_eq!(
      send_error("Pc", RemoteFault::NoProc),
      MeshRpcError::NoEndpoint("Pc".to_owned())
    );
  }

  #[test]
  fn other_send_faults_are_transport_errors() {
    assert_eq!(
      send_error("Pc", RemoteFault::MailboxFull),
      MeshRpcError::Transport("mailbox full".to_owned())
    );
  }

  #[test]
  fn a_lost_connection_is_a_node_down() {
    assert_eq!(
      down_error("Pc", &ExitReason::NoConnection),
      MeshRpcError::NodeDown
    );
  }

  #[test]
  fn an_exited_endpoint_names_its_reason() {
    assert!(matches!(
      down_error("Pc", &ExitReason::Killed),
      MeshRpcError::EndpointDown(why) if why.contains("Killed")
    ));
    assert_eq!(
      down_error("Pc", &ExitReason::NoProc),
      MeshRpcError::NoEndpoint("Pc".to_owned())
    );
  }

  #[test]
  fn an_exit_without_a_payload_is_a_protocol_error() {
    assert_eq!(
      exit_value(None),
      Err(MeshRpcError::Transport(
        "an exit without a payload".to_owned()
      ))
    );
    let garbage = exit_value(Some(&[0xff, 0xff, 0xff]));
    assert_eq!(
      garbage.map_err(|e| e.to_string().starts_with("codec")),
      Err(true)
    );
  }

  #[test]
  fn events_that_end_a_call_are_typed_errors() {
    assert_eq!(
      event_error(
        "Pc",
        ClientEvent::Silent {
          node: "b@x".to_owned(),
          silent_for: Duration::from_secs(4),
        }
      ),
      MeshRpcError::PeerSilent {
        node: "b@x".to_owned(),
        silent_for: Duration::from_secs(4),
      }
    );
    assert_eq!(
      event_error("Pc", ClientEvent::Down(ExitReason::NoConnection)),
      MeshRpcError::NodeDown
    );
    assert_eq!(
      event_error("Pc", ClientEvent::Violation("v")),
      MeshRpcError::Transport("v".to_owned())
    );
    let frame = Frame::new(CallId::new(1).unwrap(), FrameKind::Item);
    assert_eq!(
      event_error("Pc", ClientEvent::Frame(frame)),
      MeshRpcError::Transport("an unexpected frame".to_owned())
    );
  }

  #[test]
  fn generated_clients_see_codec_errors_as_protocol_errors() {
    assert!(matches!(
      transport_error(MeshRpcError::Codec("x".to_owned())),
      TransportError::Protocol(_)
    ));
    assert!(matches!(
      transport_error(MeshRpcError::NodeDown),
      TransportError::Io(_)
    ));
  }

  #[test]
  fn generated_clients_see_refusals_as_refusals() {
    let rejected = MeshRpcError::Failure(json!({ "_tag": REJECTED_TAG, "status": 403 }));
    assert!(matches!(
      failure_exit(rejected),
      Err(TransportError::Refused { status: 403, .. })
    ));
    let typed = MeshRpcError::Failure(json!({ "_tag": "Mine" }));
    assert!(matches!(failure_exit(typed), Ok(WireExit::Failure { .. })));
  }

  #[test]
  fn queries_retry_only_on_a_gone_endpoint() {
    for gone in [
      MeshRpcError::NoEndpoint("Pc".to_owned()),
      MeshRpcError::EndpointDown("Killed".to_owned()),
      MeshRpcError::NodeDown,
    ] {
      assert!(gone.is_endpoint_gone(), "{gone}");
    }
    assert!(!MeshRpcError::Failure(json!({ "_tag": "Cancelled" })).is_endpoint_gone());
  }

  #[test]
  fn failures_map_to_typed_errors() {
    for error in [
      MeshRpcError::NodeDown,
      MeshRpcError::Codec("x".to_owned()),
      MeshRpcError::Failure(json!({ "_tag": REJECTED_TAG })),
      MeshRpcError::Failure(json!({ "_tag": "Mine", "status": 404 })),
    ] {
      assert_eq!(error.rejected_status(), None, "{error}");
      assert_eq!(error.reserved(), None);
    }
    assert_eq!(MeshRpcError::NodeDown.failure_tag(), None);
    assert_eq!(
      MeshRpcError::Failure(json!({})).failure_tag(),
      Some("(untagged)")
    );
    assert!(!MeshRpcError::Codec("x".to_owned()).is_endpoint_gone());
    assert!(matches!(
      transport_error(MeshRpcError::Codec("x".to_owned())),
      TransportError::Protocol(_)
    ));
    assert!(matches!(
      failure_exit(MeshRpcError::NodeDown),
      Err(TransportError::Io(_))
    ));
  }
}
