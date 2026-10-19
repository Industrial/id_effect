//! Handler shapes of the six procedure kinds, and their transport-independent erased form.
//!
//! Every kind is a method returning an `Effect<_, E, DynEnv>` (ADR 0014 §2), so dependency
//! injection, typed errors and the test executor apply to all of them. The typed traits below
//! are what [`RpcGroup`](crate::RpcGroup)'s registration methods take and what
//! `#[rpc::service]` generates against; every `Fn` of the right shape implements them:
//!
//! | Kind | Trait | Handler |
//! |---|---|---|
//! | query | [`QueryHandler`] | `Fn(I, &mut DynEnv) -> Effect<O, E, DynEnv>` |
//! | mutation | [`MutationHandler`] | `Fn(I, &mut DynEnv) -> Effect<O, E, DynEnv>` |
//! | server stream | [`ServerStreamHandler`] | `Fn(I, &mut DynEnv) -> Effect<Stream<O, E, DynEnv>, E, DynEnv>` |
//! | client stream | [`ClientStreamHandler`] | `Fn(Stream<I, E, DynEnv>, &mut DynEnv) -> Effect<O, E, DynEnv>` |
//! | bidi | [`BidiHandler`] | `Fn(&mut DynEnv) -> Effect<Duplex<I, O, E>, E, DynEnv>` |
//! | subscription | [`SubscriptionHandler`] | `Fn(I, Option<Cursor>, &mut DynEnv) -> Effect<Subscription<O, E>, E, DynEnv>` |
//!
//! `E` is a typed error union ([`RpcErrorUnion`]); inputs and outputs cross the wire through a
//! [`WireSchema`] (built from an `id_effect` [`Schema`], usually `WireSchema::of::<T>()` for a
//! `#[derive(Schema)]` type).
//!
//! Registration erases a handler into an [`ErasedHandler`]: JSON values in, a [`CallOutcome`]
//! out, stream items through an [`Outbound`] channel, client items through an [`Inbound`]
//! channel. Every transport (HTTP here; WebSocket, SSE, WebTransport and the mesh in later
//! leaves) drives these, so they share decoding, error mapping and cancellation. Each erased call
//! runs its effect as one fiber; dropping the call's future interrupts that fiber and runs its
//! finalizers.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::future::{Either, select};
use http::StatusCode;
use id_effect::schema::data::EffectData;
use id_effect::schema::serde_bridge::{serde_json_from_unknown, unknown_from_serde_json};
use id_effect::schema::{HasSchema, Schema, Unknown};
use id_effect::{Cause, DynEnv, Effect, Exit, Stream, die, from_async, succeed};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::error_union::{RpcErrorUnion, failure_value, typed_error_value};
use crate::kinds::{Duplex, Subscription};
use crate::serialization::StructWireJson;
use crate::wire::{CallId, Cursor, InputIssue, RpcFailure, WireExit, WireResult};

// ---------------------------------------------------------------------------------------------
// Wire values
// ---------------------------------------------------------------------------------------------

/// A Schema wire type that renders as JSON ([`WireSchema::new`]).
pub trait IntoWireJson {
  /// The JSON form.
  fn into_wire_json(self) -> Value;
}

impl IntoWireJson for Unknown {
  fn into_wire_json(self) -> Value {
    serde_json_from_unknown(&self)
  }
}

impl IntoWireJson for String {
  fn into_wire_json(self) -> Value {
    Value::String(self)
  }
}

impl IntoWireJson for i64 {
  fn into_wire_json(self) -> Value {
    Value::from(self)
  }
}

impl IntoWireJson for f64 {
  fn into_wire_json(self) -> Value {
    Value::from(self)
  }
}

impl IntoWireJson for bool {
  fn into_wire_json(self) -> Value {
    Value::Bool(self)
  }
}

impl IntoWireJson for () {
  fn into_wire_json(self) -> Value {
    Value::Null
  }
}

impl<T: IntoWireJson> IntoWireJson for Option<T> {
  fn into_wire_json(self) -> Value {
    self.map_or(Value::Null, IntoWireJson::into_wire_json)
  }
}

impl<T: IntoWireJson> IntoWireJson for Vec<T> {
  fn into_wire_json(self) -> Value {
    Value::Array(self.into_iter().map(IntoWireJson::into_wire_json).collect())
  }
}

type DecodeFn<T> = Arc<dyn Fn(&Value) -> Result<T, Vec<InputIssue>> + Send + Sync>;
type EncodeFn<T> = Arc<dyn Fn(T) -> Result<Value, String> + Send + Sync>;

/// How one input, output or item type crosses the wire: decode from JSON (every Schema issue at
/// once), encode to JSON, and its JSON Schema for the contract.
pub struct WireSchema<T> {
  decode: DecodeFn<T>,
  encode: EncodeFn<T>,
  json_schema: Value,
  typed: Option<crate::codec::TypedCodec>,
}

impl<T> Clone for WireSchema<T> {
  fn clone(&self) -> Self {
    Self {
      decode: self.decode.clone(),
      encode: self.encode.clone(),
      json_schema: self.json_schema.clone(),
      typed: self.typed.clone(),
    }
  }
}

impl<T> std::fmt::Debug for WireSchema<T> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("WireSchema")
      .field("json_schema", &self.json_schema)
      .finish_non_exhaustive()
  }
}

/// A dot-separated Schema path as a JSON Pointer (`items.0.name` → `/items/0/name`).
pub fn json_pointer(path: &str) -> String {
  if path.is_empty() {
    return String::new();
  }
  path.split('.').fold(String::new(), |mut out, segment| {
    out.push('/');
    out.push_str(&segment.replace('~', "~0").replace('/', "~1"));
    out
  })
}

pub(crate) fn decode_with<T, I, E>(
  schema: &Schema<T, I, E>,
  value: &Value,
) -> Result<T, Vec<InputIssue>> {
  schema
    .decode_unknown_all(&unknown_from_serde_json(value.clone()))
    .map_err(|errors| {
      errors
        .issues
        .into_iter()
        .map(|e| InputIssue {
          path: json_pointer(&e.path),
          message: e.message,
        })
        .collect()
    })
}

impl<T: 'static> WireSchema<T> {
  /// From any Schema whose wire type renders as JSON ([`IntoWireJson`]: `Unknown`, the
  /// primitives, `Option`, `Vec`).
  pub fn new<I, E>(schema: Schema<T, I, E>) -> Self
  where
    I: IntoWireJson + 'static,
    E: EffectData + 'static,
  {
    let json_schema = schema.json_schema();
    let schema = Arc::new(schema);
    let encoder = schema.clone();
    Self {
      decode: Arc::new(move |v| decode_with(&schema, v)),
      encode: Arc::new(move |t| Ok(encoder.encode(t).into_wire_json())),
      json_schema,
      typed: None,
    }
  }

  /// `T`'s own schema (a `#[derive(Schema)]` type).
  pub fn of() -> Self
  where
    T: HasSchema<A = T>,
    T::I: IntoWireJson,
  {
    Self::new(T::schema())
  }

  /// A tuple Schema (`schema::struct_`) encoded as an object whose members are named by
  /// `fields`, in order.
  pub fn with_fields<I, E>(schema: Schema<T, I, E>, fields: &'static [&'static str]) -> Self
  where
    I: StructWireJson + 'static,
    E: EffectData + 'static,
  {
    let json_schema = schema.json_schema();
    let schema = Arc::new(schema);
    let encoder = schema.clone();
    Self {
      decode: Arc::new(move |v| decode_with(&schema, v)),
      encode: Arc::new(move |t| encoder.encode(t).to_json_object(fields)),
      json_schema,
      typed: None,
    }
  }

  /// From its three parts (a codec bridge, or a type without a Schema).
  pub fn from_fns(
    decode: impl Fn(&Value) -> Result<T, Vec<InputIssue>> + Send + Sync + 'static,
    encode: impl Fn(T) -> Result<Value, String> + Send + Sync + 'static,
    json_schema: Value,
  ) -> Self {
    Self {
      decode: Arc::new(decode),
      encode: Arc::new(encode),
      json_schema,
      typed: None,
    }
  }

  /// Also carry `T` through serde, so codecs that are not self-describing (postcard) can
  /// decode and encode it ([`TypedCodec`](crate::codec::TypedCodec)). The Schema still
  /// validates what such a codec decodes. `T`'s serde form is its binary wire form.
  #[must_use]
  pub fn with_serde(mut self) -> Self
  where
    T: serde::Serialize + serde::de::DeserializeOwned,
  {
    self.typed = None;
    self.typed = Some(crate::codec::TypedCodec::of(&self));
    self
  }

  /// The typed bridge ([`WireSchema::with_serde`]), if any.
  pub fn typed_codec(&self) -> Option<&crate::codec::TypedCodec> {
    self.typed.as_ref()
  }

  /// Decode `value`, collecting every issue.
  pub fn decode(&self, value: &Value) -> Result<T, Vec<InputIssue>> {
    (self.decode)(value)
  }

  /// Encode `value`.
  pub fn encode(&self, value: T) -> Result<Value, String> {
    (self.encode)(value)
  }

  /// The JSON Schema (2020-12).
  pub fn json_schema(&self) -> &Value {
    &self.json_schema
  }
}

// ---------------------------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------------------------

/// What every erased call receives: the environment it runs with and the correlation id that
/// names it in logs (and in `InternalError { id }`).
#[derive(Clone)]
pub struct CallContext {
  /// The environment the handler's effect runs with.
  pub env: DynEnv,
  /// The request's correlation id.
  pub correlation_id: Arc<str>,
  /// The [`RequestCache`](id_effect::RequestCache) a query or mutation runs with (one per
  /// batch, so the batch's duplicate loads through a `RequestResolver` are merged); `None`:
  /// caching as the handler sets it.
  pub request_cache: Option<id_effect::RequestCache>,
}

impl CallContext {
  /// A context for one call.
  pub fn new(env: DynEnv, correlation_id: impl Into<Arc<str>>) -> Self {
    Self {
      env,
      correlation_id: correlation_id.into(),
      request_cache: None,
    }
  }

  /// Run queries and mutations of this context with `cache` as their ambient request cache.
  #[must_use]
  pub fn with_request_cache(mut self, cache: id_effect::RequestCache) -> Self {
    self.request_cache = Some(cache);
    self
  }
}

impl std::fmt::Debug for CallContext {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CallContext")
      .field("correlation_id", &self.correlation_id)
      .finish_non_exhaustive()
  }
}

/// How one call ended: its [`WireExit`] and the HTTP status a single call answers with.
#[derive(Debug, Clone, PartialEq)]
pub struct CallOutcome {
  /// The exit.
  pub exit: WireExit,
  /// `200` on success; the error's status ([`crate::error_union::RpcErrorStatus`]) or the
  /// reserved failure's ([`RpcFailure::status`]) otherwise.
  pub status: StatusCode,
}

impl CallOutcome {
  /// A success.
  pub fn success(value: Value) -> Self {
    Self {
      exit: WireExit::Success { value },
      status: StatusCode::OK,
    }
  }

  /// A typed error (an already checked union member).
  pub fn typed(error: Value, status: StatusCode) -> Self {
    Self {
      exit: WireExit::Failure { error },
      status,
    }
  }

  /// A reserved failure.
  pub fn failure(failure: RpcFailure) -> Self {
    Self {
      status: failure.status(),
      exit: WireExit::Failure {
        error: failure_value(&failure),
      },
    }
  }

  /// `InternalError { id }`: a defect or an unexpected interrupt, the correlation id only.
  pub fn internal(correlation_id: &str) -> Self {
    Self::failure(RpcFailure::InternalError {
      id: correlation_id.to_owned(),
    })
  }

  /// `InvalidInput { issues }`.
  pub fn invalid_input(issues: Vec<InputIssue>) -> Self {
    Self::failure(RpcFailure::InvalidInput { issues })
  }

  /// The reserved failure this outcome is, if any.
  pub fn reserved(&self) -> Option<RpcFailure> {
    match &self.exit {
      WireExit::Failure { error } => {
        let tag = error.get("_tag").and_then(Value::as_str)?;
        if RpcFailure::is_reserved(tag) {
          serde_json::from_value(error.clone()).ok()
        } else {
          None
        }
      }
      WireExit::Success { .. } => None,
    }
  }

  /// Whether a stored copy may answer a retry (`Idempotency-Key`): a success, a typed error or
  /// `InvalidInput` — not a fault, a timeout, a cancel or an exhausted limit, which a retry
  /// should run again.
  pub fn is_replayable(&self) -> bool {
    !matches!(
      self.reserved(),
      Some(
        RpcFailure::InternalError { .. }
          | RpcFailure::DeadlineExceeded
          | RpcFailure::Cancelled
          | RpcFailure::ResourceExhausted { .. }
      )
    )
  }

  /// The [`WireResult`] of call `id` to `tag`.
  pub fn into_result(self, id: Option<CallId>, tag: impl Into<String>) -> WireResult {
    WireResult::new(id, tag, self.exit)
  }
}

/// The first typed failure in `cause` (depth-first, left to right), else the rest.
fn split_cause<E>(cause: Cause<E>) -> Result<E, Cause<E>> {
  match cause {
    Cause::Fail(e) => Ok(e),
    Cause::Both(l, r) => match split_cause(*l) {
      Ok(e) => Ok(e),
      Err(l) => split_cause(*r).map_err(|r| Cause::Both(Box::new(l), Box::new(r))),
    },
    Cause::Then(l, r) => match split_cause(*l) {
      Ok(e) => Ok(e),
      Err(l) => split_cause(*r).map_err(|r| Cause::Then(Box::new(l), Box::new(r))),
    },
    other => Err(other),
  }
}

fn first_defect<E>(cause: &Cause<E>) -> Option<&str> {
  match cause {
    Cause::Die(d) => Some(d.message()),
    Cause::Both(l, r) | Cause::Then(l, r) => first_defect(l).or_else(|| first_defect(r)),
    Cause::Fail(_) | Cause::Interrupt(_) => None,
  }
}

/// The outcome of a failed handler fiber: its typed error, or `InternalError { id }` for a
/// defect, an unexpected interrupt or an error that is not a valid union member (logged here
/// with the correlation id; nothing but the id reaches the client).
pub fn outcome_from_cause<E: RpcErrorUnion>(cause: Cause<E>, tag: &str, cid: &str) -> CallOutcome {
  match split_cause(cause) {
    Ok(error) => match typed_error_value(&error) {
      Ok((value, status)) => CallOutcome::typed(value, status),
      Err(why) => {
        let error = error.to_wire();
        tracing::error!(
          correlation_id = cid,
          rpc.tag = tag,
          reason = %why,
          error = %error,
          "rpc handler error answered as InternalError"
        );
        CallOutcome::internal(cid)
      }
    },
    Err(rest) => {
      let defect = first_defect(&rest).unwrap_or("interrupted");
      tracing::error!(
        correlation_id = cid,
        rpc.tag = tag,
        defect,
        "rpc handler defect answered as InternalError"
      );
      CallOutcome::internal(cid)
    }
  }
}

fn outcome_from_exit<O, E: RpcErrorUnion>(
  exit: Exit<O, E>,
  output: &WireSchema<O>,
  tag: &str,
  cid: &str,
) -> CallOutcome
where
  O: 'static,
{
  match exit {
    Exit::Success(value) => match output.encode(value) {
      Ok(value) => CallOutcome::success(value),
      Err(why) => {
        tracing::error!(correlation_id = cid, rpc.tag = tag, reason = %why, "rpc output does not encode");
        CallOutcome::internal(cid)
      }
    },
    Exit::Failure(cause) => outcome_from_cause(cause, tag, cid),
  }
}

fn unit_outcome<E: RpcErrorUnion>(exit: Exit<(), E>, tag: &str, cid: &str) -> CallOutcome {
  match exit {
    Exit::Success(()) => CallOutcome::success(Value::Null),
    Exit::Failure(cause) => outcome_from_cause(cause, tag, cid),
  }
}

// ---------------------------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------------------------

/// One item a stream, two-way stream or subscription sends.
#[derive(Debug, Clone, PartialEq)]
pub struct OutItem {
  /// The resume position (subscriptions and resumable streams).
  pub cursor: Option<Cursor>,
  /// The encoded item.
  pub value: Value,
}

/// Where a running call sends its items: a bounded channel, so a transport that stops reading
/// suspends the producer (pull-based backpressure).
#[derive(Debug, Clone)]
pub struct Outbound {
  tx: mpsc::Sender<OutItem>,
}

/// The transport's end of an [`Outbound`].
#[derive(Debug)]
pub struct OutboundReceiver {
  rx: mpsc::Receiver<OutItem>,
}

/// An [`Outbound`] holding at most `window` (≥ 1) unread items.
pub fn outbound(window: usize) -> (Outbound, OutboundReceiver) {
  let (tx, rx) = mpsc::channel(window.max(1));
  (Outbound { tx }, OutboundReceiver { rx })
}

impl Outbound {
  /// Send `item`, waiting for room; `false` when the receiver is gone.
  pub async fn send(&self, item: OutItem) -> bool {
    self.tx.send(item).await.is_ok()
  }
}

impl OutboundReceiver {
  /// The next item; `None` once every [`Outbound`] is gone and the buffer is drained.
  pub async fn recv(&mut self) -> Option<OutItem> {
    self.rx.recv().await
  }

  /// Poll for the next item (see [`OutboundReceiver::recv`]).
  pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<OutItem>> {
    self.rx.poll_recv(cx)
  }

  /// Refuse further items; buffered ones can still be received.
  pub fn close(&mut self) {
    self.rx.close();
  }
}

/// The items a client sends to a client stream or a two-way stream, before decoding.
#[derive(Debug)]
pub struct Inbound {
  rx: mpsc::Receiver<Value>,
}

/// The transport's end of an [`Inbound`]: dropping every sender ends the input.
#[derive(Debug, Clone)]
pub struct InboundSender {
  tx: mpsc::Sender<Value>,
}

/// An [`Inbound`] buffering at most `capacity` (≥ 1) items.
pub fn inbound(capacity: usize) -> (InboundSender, Inbound) {
  let (tx, rx) = mpsc::channel(capacity.max(1));
  (InboundSender { tx }, Inbound { rx })
}

impl Inbound {
  /// The next buffered item, without waiting (`None`: nothing buffered, or the input ended).
  pub fn try_recv(&mut self) -> Option<Value> {
    self.rx.try_recv().ok()
  }
}

impl InboundSender {
  /// Send one item, waiting for room; `false` once the call stopped reading.
  pub async fn send(&self, item: Value) -> bool {
    self.tx.send(item).await.is_ok()
  }
}

/// Reports the first input item that failed to decode.
#[derive(Clone)]
struct BadInput(Arc<Mutex<Option<oneshot::Sender<Vec<InputIssue>>>>>);

impl BadInput {
  fn new() -> (Self, oneshot::Receiver<Vec<InputIssue>>) {
    let (tx, rx) = oneshot::channel();
    (Self(Arc::new(Mutex::new(Some(tx)))), rx)
  }

  fn report(&self, issues: Vec<InputIssue>) {
    let sender = self
      .0
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take();
    if let Some(tx) = sender {
      let _ = tx.send(issues);
    }
  }
}

/// The decoded items of `inbound` as a stream. An item that does not decode is reported to
/// `bad` and the stream stops there (the call then answers `InvalidInput` and is interrupted).
fn typed_inbound<I, E>(
  inbound: Inbound,
  schema: WireSchema<I>,
  bad: BadInput,
) -> Stream<I, E, DynEnv>
where
  I: Send + 'static,
  E: Send + 'static,
{
  let rx = Arc::new(tokio::sync::Mutex::new(inbound.rx));
  Stream::from_pull(move |_max| {
    let rx = rx.clone();
    let schema = schema.clone();
    let bad = bad.clone();
    from_async(move || async move {
      let next = rx.lock().await.recv().await;
      match next {
        None => Ok(None),
        Some(value) => match schema.decode(&value) {
          Ok(item) => Ok(Some(vec![item])),
          Err(issues) => {
            bad.report(issues);
            std::future::pending().await
          }
        },
      }
    })
  })
}

/// Items pulled per step when a stream is pumped into an [`Outbound`].
const PUMP_CHUNK: usize = 16;

/// Pull `stream` and send every item to `out`, in order, until it ends or fails.
fn pump<T, E>(
  stream: Stream<(Option<Cursor>, T), E, DynEnv>,
  out: Outbound,
  schema: WireSchema<T>,
) -> Effect<(), E, DynEnv>
where
  T: Send + 'static,
  E: Send + 'static,
{
  stream
    .next_chunk_with_size(PUMP_CHUNK)
    .flat_map(move |next| match next {
      None => succeed(()),
      Some((chunk, rest)) => {
        let mut items = Vec::new();
        for (cursor, item) in chunk.into_vec() {
          match schema.encode(item) {
            Ok(value) => items.push(OutItem { cursor, value }),
            Err(why) => return die(format!("rpc stream item does not encode: {why}")),
          }
        }
        let sink = out.clone();
        from_async(move || async move {
          for item in items {
            if !sink.send(item).await {
              // The transport is gone; it drops this call, which interrupts the fiber.
              std::future::pending::<()>().await;
            }
          }
          Ok(())
        })
        .flat_map(move |()| pump(rest, out, schema))
      }
    })
}

// ---------------------------------------------------------------------------------------------
// Typed handler shapes
// ---------------------------------------------------------------------------------------------

/// A query: one input, one output, retry-safe.
pub trait QueryHandler<I, O, E>: Send + Sync + 'static {
  /// Build the call's effect.
  fn call(&self, input: I, env: &mut DynEnv) -> Effect<O, E, DynEnv>;
}

impl<F, I, O, E> QueryHandler<I, O, E> for F
where
  F: Fn(I, &mut DynEnv) -> Effect<O, E, DynEnv> + Send + Sync + 'static,
{
  fn call(&self, input: I, env: &mut DynEnv) -> Effect<O, E, DynEnv> {
    self(input, env)
  }
}

/// A mutation: one input, one output, state-changing.
pub trait MutationHandler<I, O, E>: Send + Sync + 'static {
  /// Build the call's effect.
  fn call(&self, input: I, env: &mut DynEnv) -> Effect<O, E, DynEnv>;
}

impl<F, I, O, E> MutationHandler<I, O, E> for F
where
  F: Fn(I, &mut DynEnv) -> Effect<O, E, DynEnv> + Send + Sync + 'static,
{
  fn call(&self, input: I, env: &mut DynEnv) -> Effect<O, E, DynEnv> {
    self(input, env)
  }
}

/// A server stream: one input, a pull-based stream of outputs.
pub trait ServerStreamHandler<I, O, E>: Send + Sync + 'static {
  /// Build the call's effect, which yields the stream.
  fn call(&self, input: I, env: &mut DynEnv) -> Effect<Stream<O, E, DynEnv>, E, DynEnv>;
}

impl<F, I, O, E> ServerStreamHandler<I, O, E> for F
where
  F: Fn(I, &mut DynEnv) -> Effect<Stream<O, E, DynEnv>, E, DynEnv> + Send + Sync + 'static,
{
  fn call(&self, input: I, env: &mut DynEnv) -> Effect<Stream<O, E, DynEnv>, E, DynEnv> {
    self(input, env)
  }
}

/// A client stream: a stream of inputs, one output.
pub trait ClientStreamHandler<I, O, E>: Send + Sync + 'static {
  /// Build the call's effect from the client's items.
  fn call(&self, input: Stream<I, E, DynEnv>, env: &mut DynEnv) -> Effect<O, E, DynEnv>;
}

impl<F, I, O, E> ClientStreamHandler<I, O, E> for F
where
  F: Fn(Stream<I, E, DynEnv>, &mut DynEnv) -> Effect<O, E, DynEnv> + Send + Sync + 'static,
{
  fn call(&self, input: Stream<I, E, DynEnv>, env: &mut DynEnv) -> Effect<O, E, DynEnv> {
    self(input, env)
  }
}

/// A two-way stream: the handler yields a [`Duplex`] that maps the client's items to the
/// server's.
pub trait BidiHandler<I, O, E>: Send + Sync + 'static {
  /// Build the call's effect, which yields the duplex.
  fn call(&self, env: &mut DynEnv) -> Effect<Duplex<I, O, E>, E, DynEnv>;
}

impl<F, I, O, E> BidiHandler<I, O, E> for F
where
  F: Fn(&mut DynEnv) -> Effect<Duplex<I, O, E>, E, DynEnv> + Send + Sync + 'static,
{
  fn call(&self, env: &mut DynEnv) -> Effect<Duplex<I, O, E>, E, DynEnv> {
    self(env)
  }
}

/// A subscription: one input and the cursor to resume after (`None`: from now), a resumable
/// stream of items.
pub trait SubscriptionHandler<I, O, E>: Send + Sync + 'static {
  /// Build the call's effect, which yields the subscription.
  fn call(
    &self,
    input: I,
    after: Option<Cursor>,
    env: &mut DynEnv,
  ) -> Effect<Subscription<O, E>, E, DynEnv>;
}

impl<F, I, O, E> SubscriptionHandler<I, O, E> for F
where
  F: Fn(I, Option<Cursor>, &mut DynEnv) -> Effect<Subscription<O, E>, E, DynEnv>
    + Send
    + Sync
    + 'static,
{
  fn call(
    &self,
    input: I,
    after: Option<Cursor>,
    env: &mut DynEnv,
  ) -> Effect<Subscription<O, E>, E, DynEnv> {
    self(input, after, env)
  }
}

// ---------------------------------------------------------------------------------------------
// Erased handlers
// ---------------------------------------------------------------------------------------------

/// A boxed, `Send` call future.
pub type BoxCall = Pin<Box<dyn Future<Output = CallOutcome> + Send + 'static>>;

/// Erased query or mutation: the input, then the outcome.
pub type UnaryFn = Arc<dyn Fn(Value, CallContext) -> BoxCall + Send + Sync>;
/// Erased server stream: the input; items to the [`Outbound`]; then the outcome.
pub type ServerStreamFn = Arc<dyn Fn(Value, CallContext, Outbound) -> BoxCall + Send + Sync>;
/// Erased client stream: items from the [`Inbound`]; then the outcome.
pub type ClientStreamFn = Arc<dyn Fn(Inbound, CallContext) -> BoxCall + Send + Sync>;
/// Erased two-way stream: items from the [`Inbound`], items to the [`Outbound`]; then the
/// outcome.
pub type BidiFn = Arc<dyn Fn(Inbound, CallContext, Outbound) -> BoxCall + Send + Sync>;
/// Erased subscription: the input and the resume cursor; items to the [`Outbound`]; then the
/// outcome.
pub type SubscriptionFn =
  Arc<dyn Fn(Value, Option<Cursor>, CallContext, Outbound) -> BoxCall + Send + Sync>;

/// A registered handler, by kind.
#[derive(Clone)]
pub enum ErasedHandler {
  /// Query or mutation.
  Unary(UnaryFn),
  /// Server stream.
  ServerStream(ServerStreamFn),
  /// Client stream.
  ClientStream(ClientStreamFn),
  /// Two-way stream.
  Bidi(BidiFn),
  /// Subscription.
  Subscription(SubscriptionFn),
}

impl std::fmt::Debug for ErasedHandler {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(match self {
      Self::Unary(_) => "Unary",
      Self::ServerStream(_) => "ServerStream",
      Self::ClientStream(_) => "ClientStream",
      Self::Bidi(_) => "Bidi",
      Self::Subscription(_) => "Subscription",
    })
  }
}

/// Erase a query or mutation handler (`build` is [`QueryHandler::call`] or
/// [`MutationHandler::call`]).
pub(crate) fn erase_unary<I, O, E, B>(
  tag: Arc<str>,
  input: WireSchema<I>,
  output: WireSchema<O>,
  build: B,
) -> UnaryFn
where
  I: Send + 'static,
  O: Send + 'static,
  E: RpcErrorUnion,
  B: Fn(I, &mut DynEnv) -> Effect<O, E, DynEnv> + Send + Sync + 'static,
{
  let build = Arc::new(build);
  Arc::new(move |payload, ctx: CallContext| {
    let tag = tag.clone();
    let input = input.clone();
    let output = output.clone();
    let build = build.clone();
    Box::pin(async move {
      let value = match input.decode(&payload) {
        Ok(value) => value,
        Err(issues) => return CallOutcome::invalid_input(issues),
      };
      let cache = ctx.request_cache;
      let exit = id_effect_axum::run_with_env(ctx.env, move |env| {
        let effect = build(value, env);
        match cache {
          Some(cache) => cache.provide(effect),
          None => effect,
        }
      })
      .await;
      outcome_from_exit(exit, &output, &tag, &ctx.correlation_id)
    })
  })
}

/// Erase a server stream handler.
pub(crate) fn erase_server_stream<I, O, E, H>(
  tag: Arc<str>,
  input: WireSchema<I>,
  item: WireSchema<O>,
  handler: H,
) -> ServerStreamFn
where
  I: Send + 'static,
  O: Send + 'static,
  E: RpcErrorUnion,
  H: ServerStreamHandler<I, O, E>,
{
  let handler = Arc::new(handler);
  Arc::new(move |payload, ctx: CallContext, out| {
    let tag = tag.clone();
    let input = input.clone();
    let item = item.clone();
    let handler = handler.clone();
    Box::pin(async move {
      let value = match input.decode(&payload) {
        Ok(value) => value,
        Err(issues) => return CallOutcome::invalid_input(issues),
      };
      let exit = id_effect_axum::run_with_env(ctx.env, move |env| {
        handler
          .call(value, env)
          .flat_map(move |stream| pump(stream.map_serial(|o| (None, o)), out, item))
      })
      .await;
      unit_outcome(exit, &tag, &ctx.correlation_id)
    })
  })
}

/// Run `run` until it ends, or until an input item fails to decode (`InvalidInput`; `run` is
/// dropped, which interrupts its fiber).
async fn with_bad_input<F>(run: F, bad: oneshot::Receiver<Vec<InputIssue>>) -> CallOutcome
where
  F: Future<Output = CallOutcome> + Send,
{
  match select(Box::pin(run), bad).await {
    Either::Left((outcome, _)) => outcome,
    Either::Right((Ok(issues), run)) => {
      drop(run);
      CallOutcome::invalid_input(issues)
    }
    // Every reporter is gone without a report: no item failed.
    Either::Right((Err(_), run)) => run.await,
  }
}

/// Erase a client stream handler.
pub(crate) fn erase_client_stream<I, O, E, H>(
  tag: Arc<str>,
  item: WireSchema<I>,
  output: WireSchema<O>,
  handler: H,
) -> ClientStreamFn
where
  I: Send + 'static,
  O: Send + 'static,
  E: RpcErrorUnion,
  H: ClientStreamHandler<I, O, E>,
{
  let handler = Arc::new(handler);
  Arc::new(move |inbound, ctx: CallContext| {
    let tag = tag.clone();
    let item = item.clone();
    let output = output.clone();
    let handler = handler.clone();
    Box::pin(async move {
      let (bad, bad_rx) = BadInput::new();
      let stream = typed_inbound(inbound, item, bad);
      let cid = ctx.correlation_id.clone();
      let run = async move {
        let exit =
          id_effect_axum::run_with_env(ctx.env, move |env| handler.call(stream, env)).await;
        outcome_from_exit(exit, &output, &tag, &cid)
      };
      with_bad_input(run, bad_rx).await
    })
  })
}

/// Erase a two-way stream handler.
pub(crate) fn erase_bidi<I, O, E, H>(
  tag: Arc<str>,
  input: WireSchema<I>,
  output: WireSchema<O>,
  handler: H,
) -> BidiFn
where
  I: Send + 'static,
  O: Send + 'static,
  E: RpcErrorUnion,
  H: BidiHandler<I, O, E>,
{
  let handler = Arc::new(handler);
  Arc::new(move |inbound, ctx: CallContext, out| {
    let tag = tag.clone();
    let input = input.clone();
    let output = output.clone();
    let handler = handler.clone();
    Box::pin(async move {
      let (bad, bad_rx) = BadInput::new();
      let stream = typed_inbound(inbound, input, bad);
      let cid = ctx.correlation_id.clone();
      let run = async move {
        let exit = id_effect_axum::run_with_env(ctx.env, move |env| {
          handler
            .call(env)
            .flat_map(move |duplex| pump(duplex.run(stream).map_serial(|o| (None, o)), out, output))
        })
        .await;
        unit_outcome(exit, &tag, &cid)
      };
      with_bad_input(run, bad_rx).await
    })
  })
}

/// Erase a subscription handler.
pub(crate) fn erase_subscription<I, O, E, H>(
  tag: Arc<str>,
  input: WireSchema<I>,
  item: WireSchema<O>,
  handler: H,
) -> SubscriptionFn
where
  I: Send + 'static,
  O: Send + 'static,
  E: RpcErrorUnion,
  H: SubscriptionHandler<I, O, E>,
{
  let handler = Arc::new(handler);
  Arc::new(move |payload, after, ctx: CallContext, out| {
    let tag = tag.clone();
    let input = input.clone();
    let item = item.clone();
    let handler = handler.clone();
    Box::pin(async move {
      let value = match input.decode(&payload) {
        Ok(value) => value,
        Err(issues) => return CallOutcome::invalid_input(issues),
      };
      let exit = id_effect_axum::run_with_env(ctx.env, move |env| {
        handler
          .call(value, after, env)
          .flat_map(move |subscription| {
            let items = subscription
              .into_stream()
              .map_serial(|i| (i.cursor, i.value));
            pump(items, out, item)
          })
      })
      .await;
      unit_outcome(exit, &tag, &ctx.correlation_id)
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{Never, schema};
  use serde_json::json;

  #[test]
  fn wire_json_renders_schema_wire_types() {
    assert_eq!(Unknown::I64(3).into_wire_json(), json!(3));
    assert_eq!("a".to_owned().into_wire_json(), json!("a"));
    assert_eq!(2_i64.into_wire_json(), json!(2));
    assert_eq!(0.5_f64.into_wire_json(), json!(0.5));
    assert_eq!(true.into_wire_json(), json!(true));
    assert_eq!(().into_wire_json(), Value::Null);
    assert_eq!(None::<bool>.into_wire_json(), Value::Null);
    assert_eq!(Some(1_i64).into_wire_json(), json!(1));
    assert_eq!(vec![1_i64, 2].into_wire_json(), json!([1, 2]));
    assert_eq!(json_pointer(""), "");
    assert_eq!(json_pointer("a.0~/b"), "/a/0~0~1b");
  }

  #[test]
  fn wire_schemas_decode_with_pointers_and_encode() {
    let pair = WireSchema::with_fields(
      schema::struct_("name", schema::string::<()>(), "n", schema::i64::<()>()),
      &["name", "n"],
    );
    assert_eq!(
      pair.encode(("a".into(), 1)).unwrap(),
      json!({"name": "a", "n": 1})
    );
    let issues = pair.decode(&json!({"name": 1, "n": "x"})).unwrap_err();
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].path, "/name");
    assert_eq!(pair.json_schema()["type"], "object");
    assert!(format!("{pair:?}").contains("WireSchema"));
    let text = WireSchema::new(schema::string::<()>());
    assert_eq!(text.clone().encode("x".into()).unwrap(), json!("x"));
    let raw = WireSchema::from_fns(|v: &Value| Ok(v.clone()), Ok, json!({}));
    assert_eq!(raw.decode(&json!(1)).unwrap(), json!(1));
  }

  #[test]
  fn outcomes_classify_reserved_failures() {
    let ok = CallOutcome::success(json!(1));
    assert!(ok.reserved().is_none() && ok.is_replayable());
    let typed = CallOutcome::typed(json!({"_tag": "NotFound"}), StatusCode::NOT_FOUND);
    assert!(typed.reserved().is_none() && typed.is_replayable());
    let invalid = CallOutcome::invalid_input(vec![]);
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(invalid.is_replayable());
    for failure in [
      RpcFailure::InternalError { id: "c".into() },
      RpcFailure::DeadlineExceeded,
      RpcFailure::Cancelled,
      RpcFailure::ResourceExhausted {
        limit: "items".into(),
      },
    ] {
      let outcome = CallOutcome::failure(failure.clone());
      assert_eq!(outcome.reserved(), Some(failure));
      assert!(!outcome.is_replayable());
    }
    let untagged = CallOutcome::typed(json!("x"), StatusCode::BAD_REQUEST);
    assert!(untagged.reserved().is_none());
    let result = CallOutcome::internal("cid").into_result(None, "A.b");
    assert_eq!(
      serde_json::to_value(&result).unwrap(),
      json!({"tag": "A.b", "exit": {"_tag": "Failure", "error": {"_tag": "InternalError", "id": "cid"}}})
    );
  }

  #[test]
  fn causes_split_into_typed_errors_or_internal() {
    let both: Cause<crate::RpcError> = Cause::both(
      Cause::die("boom"),
      Cause::then(
        Cause::interrupt(id_effect::FiberId::fresh()),
        Cause::fail(crate::RpcError::not_found("x")),
      ),
    );
    let outcome = outcome_from_cause(both, "A.b", "cid");
    assert_eq!(outcome.status, StatusCode::NOT_FOUND);
    let defect: Cause<crate::RpcError> = Cause::then(
      Cause::interrupt(id_effect::FiberId::fresh()),
      Cause::die("x"),
    );
    assert_eq!(
      outcome_from_cause(defect, "A.b", "cid").reserved(),
      Some(RpcFailure::InternalError { id: "cid".into() })
    );
    let internal = Cause::fail(crate::RpcError::internal("secret"));
    let outcome = outcome_from_cause(internal, "A.b", "cid");
    assert!(
      !serde_json::to_string(&outcome.exit)
        .unwrap()
        .contains("secret")
    );
  }

  #[tokio::test]
  async fn channels_carry_items_and_close() {
    let (out, mut rx) = outbound(0);
    let item = OutItem {
      cursor: Some(Cursor::events(1)),
      value: json!(1),
    };
    assert!(out.send(item.clone()).await);
    assert_eq!(rx.recv().await, Some(item));
    rx.close();
    assert!(
      !out
        .send(OutItem {
          cursor: None,
          value: json!(2)
        })
        .await
    );
    let (tx, inbound) = inbound(0);
    drop(inbound);
    assert!(!tx.send(json!(1)).await);
    let ctx = CallContext::new(DynEnv::new(), "cid");
    assert!(format!("{ctx:?}").contains("cid"));
    let handler = ErasedHandler::Unary(Arc::new(|_, _| {
      Box::pin(async { CallOutcome::success(Value::Null) }) as BoxCall
    }));
    assert_eq!(format!("{handler:?}"), "Unary");
    let done = || Box::pin(async { CallOutcome::success(Value::Null) }) as BoxCall;
    let names: Vec<String> = [
      ErasedHandler::ServerStream(Arc::new(move |_, _, _| done())),
      ErasedHandler::ClientStream(Arc::new(move |_, _| done())),
      ErasedHandler::Bidi(Arc::new(move |_, _, _| done())),
      ErasedHandler::Subscription(Arc::new(move |_, _, _, _| done())),
    ]
    .iter()
    .map(|h| format!("{h:?}"))
    .collect();
    assert_eq!(
      names,
      ["ServerStream", "ClientStream", "Bidi", "Subscription"]
    );
  }

  #[derive(id_effect::Schema)]
  struct Todo {
    title: String,
    done: bool,
  }

  #[test]
  fn wire_schema_of_a_derived_type() {
    let todo = WireSchema::<Todo>::of();
    let value = todo
      .encode(Todo {
        title: "milk".into(),
        done: false,
      })
      .unwrap();
    assert_eq!(value, json!({"title": "milk", "done": false}));
    let back = todo.decode(&value).unwrap();
    assert_eq!((back.title.as_str(), back.done), ("milk", false));
    assert_eq!(todo.json_schema()["type"], "object");
  }

  #[test]
  fn typed_errors_on_the_left_of_composite_causes_win() {
    for cause in [
      Cause::both(
        Cause::fail(crate::RpcError::not_found("x")),
        Cause::die("later"),
      ),
      Cause::then(
        Cause::fail(crate::RpcError::not_found("x")),
        Cause::die("later"),
      ),
    ] {
      assert_eq!(
        outcome_from_cause(cause, "A.b", "cid").status,
        StatusCode::NOT_FOUND
      );
    }
    let interrupted: Cause<crate::RpcError> = Cause::interrupt(id_effect::FiberId::fresh());
    assert_eq!(
      outcome_from_cause(interrupted, "A.b", "cid").status,
      StatusCode::INTERNAL_SERVER_ERROR
    );
  }

  fn counting(n: i64) -> impl ServerStreamHandler<i64, i64, Never> {
    move |_: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(0..n))
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn stream_items_that_do_not_encode_are_defects() {
    let refuse = WireSchema::from_fns(|_| Ok(0_i64), |_| Err("no".to_owned()), json!({}));
    let call = erase_server_stream(
      Arc::from("A.s"),
      WireSchema::new(id_effect::schema::i64::<()>()),
      refuse,
      counting(3),
    );
    let (out, _rx) = outbound(4);
    let outcome = call(json!(1), CallContext::new(DynEnv::new(), "cid"), out).await;
    assert_eq!(
      outcome.reserved(),
      Some(RpcFailure::InternalError { id: "cid".into() })
    );
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_stream_whose_reader_is_gone_waits_to_be_dropped() {
    let call = erase_server_stream(
      Arc::from("A.s"),
      WireSchema::new(id_effect::schema::i64::<()>()),
      WireSchema::new(id_effect::schema::i64::<()>()),
      counting(3),
    );
    let (out, rx) = outbound(1);
    drop(rx);
    let run = call(json!(1), CallContext::new(DynEnv::new(), "cid"), out);
    let waited = tokio::time::timeout(std::time::Duration::from_millis(30), run).await;
    assert!(
      waited.is_err(),
      "the call waits for its transport to drop it"
    );
  }
}
