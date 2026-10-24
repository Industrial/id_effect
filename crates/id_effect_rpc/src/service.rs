//! What `#[rpc::service]` generates against (ADR 0014 D11, design note §1 and §4).
//!
//! The attribute ([`crate::service`](macro@crate::service)) turns a trait of procedures into a
//! service. For `trait Greeter` it emits the trait itself (the **handler trait**: every method
//! returns an `Effect<A, E, DynEnv>`), and next to it
//!
//! - `GreeterService`, an [`RpcService`] with the service's name, version and
//!   [`contract`](RpcService::contract), plus `register` / `group` / `server` / `mount`, which
//!   put an implementation of the trait in an [`RpcGroup`], an [`RpcServer`](crate::RpcServer)
//!   or a typed router;
//! - `GreeterClient<T>`, the Rust client, generic over the [`RpcTransport`] it calls through:
//!   [`LocalTransport`] (in-process, every kind) or [`HttpTransport`] (feature `client`;
//!   queries and mutations over `POST /rpc`). Each method returns an effect whose error is a
//!   [`RpcCallError`] of the procedure's typed error.
//!
//! `#[derive(RpcErrorUnion)]` ([`crate::RpcErrorUnion`](macro@crate::RpcErrorUnion)) makes an
//! enum a typed error union: it implements [`RpcErrorStatus`](crate::RpcErrorStatus),
//! [`RpcErrorUnion`] and [`RpcErrorDecode`], and refuses the reserved tags of design note §4.
//!
//! Inputs, outputs, stream items and error fields cross the wire through [`types`]: `String`,
//! `bool`, `i64`, `f64`, `()`, `Option<T>` and `Vec<T>` are built in, and any other type must
//! implement [`RpcSchema`] — which every `#[derive(id_effect::Schema)]` struct does.
//!
//! ```rust
//! use id_effect::{DynEnv, Effect, Never, fail, succeed};
//! use id_effect_rpc::service::{LocalTransport, RpcCallError, RpcService};
//! use id_effect_rpc as rpc;
//!
//! #[derive(Debug, rpc::RpcErrorUnion)]
//! enum GreetError {
//!   #[rpc(status = 404)]
//!   Unknown { name: String },
//! }
//!
//! /// Says hello.
//! #[rpc::service(version = 1)]
//! trait Greeter {
//!   /// Greets someone.
//!   #[query]
//!   #[public]
//!   fn greet(&self, name: String) -> Effect<String, GreetError, DynEnv>;
//! }
//!
//! struct Hello;
//!
//! impl Greeter for Hello {
//!   fn greet(&self, name: String) -> Effect<String, GreetError, DynEnv> {
//!     if name.is_empty() {
//!       fail(GreetError::Unknown { name })
//!     } else {
//!       succeed(format!("Hello, {name}!"))
//!     }
//!   }
//! }
//!
//! let contract = GreeterService::contract();
//! assert_eq!((contract.name.as_str(), contract.version), ("Greeter", 1));
//! assert_eq!(contract.procedures[0].description.as_deref(), Some("Greets someone."));
//!
//! let client = GreeterService::client(LocalTransport::new(GreeterService::group(Hello), DynEnv::new()));
//! # let rt = tokio::runtime::Runtime::new().unwrap();
//! # rt.block_on(async {
//! let hello = id_effect_axum::run_with_env(id_effect::HNil, move |_| client.greet("Ada".into()));
//! assert_eq!(hello.await.into_result().ok().as_deref(), Some("Hello, Ada!"));
//! # });
//! ```

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use futures_util::StreamExt as _;
use futures_util::future::{Either, select};
use http::HeaderMap;
use http::request::Parts;
use id_effect::schema::HasSchema;
use id_effect::{DynEnv, Effect, Never, Stream, fail, from_async, succeed};
use id_effect_axum::route::Problem;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::cancel::CallControl;
use crate::contract::{Contract, ProcedureKind, ServiceContract};
use crate::error::RpcError;
use crate::error_union::RpcErrorUnion;
use crate::handler::{CallContext, IntoWireJson, WireSchema};
use crate::kinds::SubscriptionItem;
use crate::registry::RpcGroup;
use crate::server::dispatch::{self, Rejection};
use crate::wire::{
  CallId, Cursor, Frame, FrameKind, INITIAL_CREDITS, InputIssue, RpcFailure, WireExit,
};

// ---------------------------------------------------------------------------------------------
// Types on the wire
// ---------------------------------------------------------------------------------------------

/// A type that crosses the RPC wire through its own Schema: every `#[derive(id_effect::Schema)]`
/// struct, and any `HasSchema<A = Self>` type whose wire form is JSON-like.
#[diagnostic::on_unimplemented(
  message = "`{Self}` cannot cross the RPC wire: it has no Schema",
  label = "a procedure input, output, stream item or error field",
  note = "derive it: `#[derive(id_effect::Schema)]` on the struct (or implement `id_effect::schema::HasSchema` with `A = Self`); `String`, `bool`, `i64`, `f64`, `()`, `Option<T>` and `Vec<T>` are built in"
)]
pub trait RpcSchema: Sized + Send + 'static {
  /// How the type is decoded, encoded and described.
  fn wire_schema() -> WireSchema<Self>;
}

impl<T> RpcSchema for T
where
  T: HasSchema<A = T> + Send + 'static,
  T::I: IntoWireJson,
{
  fn wire_schema() -> WireSchema<T> {
    WireSchema::of()
  }
}

/// `schema` without `$schema`, its `$defs` moved into `defs`: a subschema whose `#/$defs/…`
/// references resolve once `defs` sits at the root of the composite ([`with_defs`]).
fn hoist_defs(mut schema: Value, defs: &mut serde_json::Map<String, Value>) -> Value {
  if let Value::Object(map) = &mut schema {
    map.remove("$schema");
    if let Some(Value::Object(inner)) = map.remove("$defs") {
      defs.extend(inner);
    }
  }
  schema
}

/// `schema` with `defs` as its `$defs` (none when empty).
fn with_defs(mut schema: Value, defs: serde_json::Map<String, Value>) -> Value {
  if !defs.is_empty()
    && let Value::Object(map) = &mut schema
  {
    map.insert("$defs".to_owned(), Value::Object(defs));
  }
  schema
}

/// The [`WireSchema`]s the macros build for procedure types and error fields.
pub mod types {
  use id_effect::schema;
  use id_effect::schema::json_schema_export::DIALECT;
  use serde_json::{Map, Value, json};

  use super::RpcSchema;
  use crate::handler::WireSchema;
  use crate::wire::InputIssue;

  fn issue(path: String, message: &str) -> InputIssue {
    InputIssue {
      path,
      message: message.to_owned(),
    }
  }

  /// A JSON string.
  pub fn string() -> WireSchema<String> {
    WireSchema::new(schema::string::<()>())
  }

  /// A JSON integer.
  pub fn i64() -> WireSchema<i64> {
    WireSchema::new(schema::i64::<()>())
  }

  /// A JSON number.
  pub fn f64() -> WireSchema<f64> {
    WireSchema::new(schema::f64::<()>())
  }

  /// A JSON boolean.
  pub fn bool() -> WireSchema<bool> {
    WireSchema::new(schema::bool_::<()>())
  }

  /// `null` (a procedure without input or output).
  pub fn unit() -> WireSchema<()> {
    WireSchema::from_fns(
      |v| {
        if v.is_null() {
          Ok(())
        } else {
          Err(vec![issue(String::new(), "expected null")])
        }
      },
      |()| Ok(Value::Null),
      json!({"$schema": DIALECT, "type": "null"}),
    )
  }

  /// `inner` or `null` (a missing error field decodes as `null`).
  pub fn option<T: Send + 'static>(inner: WireSchema<T>) -> WireSchema<Option<T>> {
    let mut defs = Map::new();
    let item = super::hoist_defs(inner.json_schema().clone(), &mut defs);
    let json_schema = super::with_defs(
      json!({"$schema": DIALECT, "anyOf": [item, {"type": "null"}]}),
      defs,
    );
    let encoder = inner.clone();
    WireSchema::from_fns(
      move |v| {
        if v.is_null() {
          Ok(None)
        } else {
          inner.decode(v).map(Some)
        }
      },
      move |value| value.map_or(Ok(Value::Null), |v| encoder.encode(v)),
      json_schema,
    )
  }

  /// A JSON array of `inner` (every item's issues at once, paths prefixed by the index).
  pub fn vec<T: Send + 'static>(inner: WireSchema<T>) -> WireSchema<Vec<T>> {
    let mut defs = Map::new();
    let item = super::hoist_defs(inner.json_schema().clone(), &mut defs);
    let json_schema = super::with_defs(
      json!({"$schema": DIALECT, "type": "array", "items": item}),
      defs,
    );
    let encoder = inner.clone();
    WireSchema::from_fns(
      move |v| {
        let Some(items) = v.as_array() else {
          return Err(vec![issue(String::new(), "expected an array")]);
        };
        let mut out = Vec::with_capacity(items.len());
        let mut issues = Vec::new();
        for (i, item) in items.iter().enumerate() {
          match inner.decode(item) {
            Ok(value) => out.push(value),
            Err(errors) => issues.extend(
              errors
                .into_iter()
                .map(|e| issue(format!("/{i}{}", e.path), &e.message)),
            ),
          }
        }
        if issues.is_empty() {
          Ok(out)
        } else {
          Err(issues)
        }
      },
      move |values| {
        values
          .into_iter()
          .map(|v| encoder.encode(v))
          .collect::<Result<Vec<_>, _>>()
          .map(Value::Array)
      },
      json_schema,
    )
  }

  /// `T`'s own schema ([`RpcSchema`]).
  pub fn of<T: RpcSchema>() -> WireSchema<T> {
    T::wire_schema()
  }
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// Reading a typed error back from its wire value (the client side of [`RpcErrorUnion`]).
pub trait RpcErrorDecode: Sized {
  /// The error `value` (`{"_tag": …}`) stands for, `None` when it is not a member of the union.
  fn from_wire(value: &Value) -> Option<Self>;
}

/// No value is a member of the empty union.
impl RpcErrorDecode for Never {
  fn from_wire(_value: &Value) -> Option<Self> {
    None
  }
}

/// The four contract variants of the labs [`RpcError`] (`Internal` never reaches a client).
impl RpcErrorDecode for RpcError {
  fn from_wire(value: &Value) -> Option<Self> {
    let message = value.get("message").and_then(Value::as_str)?.to_owned();
    match value.get("_tag").and_then(Value::as_str)? {
      "InvalidArgument" => Some(Self::invalid_argument(message)),
      "NotFound" => Some(Self::not_found(message)),
      "AlreadyExists" => Some(Self::already_exists(message)),
      "Unauthenticated" => Some(Self::unauthenticated(message)),
      _ => None,
    }
  }
}

/// A procedure's error type: a union the server sends ([`RpcErrorUnion`]) and the generated
/// client reads back ([`RpcErrorDecode`]).
#[diagnostic::on_unimplemented(
  message = "`{Self}` is not an RPC error union",
  label = "the procedure's error type",
  note = "derive it: `#[derive(id_effect_rpc::RpcErrorUnion)]` on an enum of unit or named-field variants, or use `id_effect::Never` for a procedure that cannot fail"
)]
pub trait RpcServiceError: RpcErrorUnion + RpcErrorDecode {}

impl<T: RpcErrorUnion + RpcErrorDecode> RpcServiceError for T {}

/// Why a generated client call failed.
#[derive(Debug, thiserror::Error)]
pub enum RpcCallError<E> {
  /// The procedure failed with a member of its error union.
  #[error("the procedure failed with a typed error")]
  Typed(E),
  /// A reserved failure (`InternalError`, `InvalidInput`, `DeadlineExceeded`, …).
  #[error("rpc failure {0:?}")]
  Failure(RpcFailure),
  /// A failure that is neither reserved nor a member of the union (a newer server).
  #[error("rpc error outside the procedure's union: {0}")]
  Unexpected(Value),
  /// The output (or a stream item) did not decode.
  #[error("rpc output does not decode ({} issues)", .0.len())]
  Decode(Vec<InputIssue>),
  /// The input did not encode.
  #[error("rpc input does not encode: {0}")]
  Encode(String),
  /// The call did not reach the procedure.
  #[error(transparent)]
  Transport(#[from] TransportError),
}

impl<E> RpcCallError<E> {
  /// The typed error, if the procedure failed with one.
  pub fn typed(&self) -> Option<&E> {
    match self {
      Self::Typed(e) => Some(e),
      _ => None,
    }
  }

  /// The reserved failure, if it is one.
  pub fn failure(&self) -> Option<&RpcFailure> {
    match self {
      Self::Failure(f) => Some(f),
      _ => None,
    }
  }
}

fn call_error<E: RpcErrorDecode>(error: Value) -> RpcCallError<E> {
  let reserved = error
    .get("_tag")
    .and_then(Value::as_str)
    .is_some_and(RpcFailure::is_reserved);
  if reserved {
    return match serde_json::from_value(error.clone()) {
      Ok(failure) => RpcCallError::Failure(failure),
      Err(_) => RpcCallError::Unexpected(error),
    };
  }
  match E::from_wire(&error) {
    Some(e) => RpcCallError::Typed(e),
    None => RpcCallError::Unexpected(error),
  }
}

fn exit_output<O: 'static, E: RpcErrorDecode>(
  exit: WireExit,
  output: &WireSchema<O>,
) -> Result<O, RpcCallError<E>> {
  match exit {
    WireExit::Success { value } => output.decode(&value).map_err(RpcCallError::Decode),
    WireExit::Failure { error } => Err(call_error(error)),
  }
}

// ---------------------------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------------------------

/// A service generated by `#[rpc::service]`.
pub trait RpcService {
  /// The service name (the trait name unless `name = "…"` is given).
  const NAME: &'static str;
  /// The contract version (`version = N`).
  const VERSION: u32;

  /// This version's contract: what a registration of it serves.
  fn contract() -> ServiceContract;

  /// A contract document holding only this service ([`Contract::with_service`] adds more).
  fn document() -> Contract {
    Contract::new().with_service(Self::contract())
  }
}

// ---------------------------------------------------------------------------------------------
// Transports
// ---------------------------------------------------------------------------------------------

/// A boxed `Send` future.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
/// The frames of a running stream call: `item`… then one `exit`.
pub type FrameStream = Pin<Box<dyn futures_util::Stream<Item = Frame> + Send + 'static>>;
/// The encoded items a client sends; the sender closing ends the input.
pub type ItemReceiver = mpsc::Receiver<Value>;

/// One call as a transport sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct TransportCall {
  /// `"<Service>.<procedure>"`.
  pub tag: String,
  /// The service version.
  pub version: u32,
  /// The procedure's kind.
  pub kind: ProcedureKind,
  /// The encoded input (`null` for client streams and two-way streams).
  pub payload: Value,
}

/// Why a transport did not deliver a call.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
  /// The server refused the call before running it (unknown procedure 404, guard 401/403, …).
  #[error("refused with status {status}")]
  Refused {
    /// The HTTP status of the refusal.
    status: u16,
    /// The Problem body, when the transport has one.
    problem: Option<Box<Problem>>,
  },
  /// This transport does not carry calls of this kind.
  #[error("this transport does not carry {0} calls")]
  Unsupported(ProcedureKind),
  /// The connection failed.
  #[error("transport: {0}")]
  Io(String),
  /// The peer answered something that is not the RPC protocol.
  #[error("protocol: {0}")]
  Protocol(String),
}

fn unsupported<T: Send + 'static>(kind: ProcedureKind) -> BoxFuture<Result<T, TransportError>> {
  Box::pin(std::future::ready(Err(TransportError::Unsupported(kind))))
}

/// How a generated client reaches a server. Only [`RpcTransport::unary`] is required; the
/// stream kinds answer [`TransportError::Unsupported`] unless a transport carries them.
pub trait RpcTransport: Clone + Send + Sync + 'static {
  /// A query or mutation.
  fn unary(&self, call: TransportCall) -> BoxFuture<Result<WireExit, TransportError>>;

  /// [`unary`](Self::unary) as an effect of the calling fiber (what the generated client runs).
  /// The default awaits `unary`; `HttpTransport` (feature `client`) overrides it so that the
  /// calls a program makes in the same tick join one batch window and go out as one request.
  fn unary_effect<R: Send + Sync + 'static>(
    &self,
    call: TransportCall,
  ) -> Effect<WireExit, TransportError, R> {
    let this = self.clone();
    from_async(move || this.unary(call))
  }

  /// A server stream.
  fn server_stream(&self, call: TransportCall) -> BoxFuture<Result<FrameStream, TransportError>> {
    unsupported(call.kind)
  }

  /// A client stream fed from `items`.
  fn client_stream(
    &self,
    call: TransportCall,
    items: ItemReceiver,
  ) -> BoxFuture<Result<WireExit, TransportError>> {
    drop(items);
    unsupported(call.kind)
  }

  /// A two-way stream fed from `items`.
  fn bidi(
    &self,
    call: TransportCall,
    items: ItemReceiver,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    drop(items);
    unsupported(call.kind)
  }

  /// A subscription resuming after `after`.
  fn subscribe(
    &self,
    call: TransportCall,
    after: Option<Cursor>,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    let _ = after;
    unsupported(call.kind)
  }
}

/// Calls the procedures of a group in the same process, through the same dispatch as the
/// network transports: version lookup, guards (against [`LocalTransport::with_headers`]),
/// decoding, error mapping and cancellation. Every kind is carried.
#[derive(Clone)]
pub struct LocalTransport {
  group: Arc<RpcGroup>,
  env: DynEnv,
  headers: HeaderMap,
  next_id: Arc<AtomicU64>,
}

impl std::fmt::Debug for LocalTransport {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("LocalTransport")
      .field("procedures", &self.group.len())
      .finish_non_exhaustive()
  }
}

impl LocalTransport {
  /// Call `group`'s procedures with `env`.
  pub fn new(group: RpcGroup, env: DynEnv) -> Self {
    Self {
      group: Arc::new(group),
      env,
      headers: HeaderMap::new(),
      next_id: Arc::new(AtomicU64::new(1)),
    }
  }

  /// Present `headers` to the procedures' guards.
  #[must_use]
  pub fn with_headers(mut self, headers: HeaderMap) -> Self {
    self.headers = headers;
    self
  }

  fn parts(&self) -> Parts {
    let mut parts = http::Request::new(()).into_parts().0;
    parts.headers = self.headers.clone();
    parts
  }

  fn next(&self) -> u64 {
    self.next_id.fetch_add(1, Ordering::Relaxed)
  }

  fn ctx(&self) -> CallContext {
    CallContext::new(self.env.clone(), format!("local-{}", self.next()))
  }

  fn call_id(&self) -> CallId {
    CallId::new(self.next()).unwrap_or(CallId::CONNECTION)
  }
}

fn rejection(rejection: Rejection) -> TransportError {
  let status = match rejection {
    Rejection::UnknownProcedure { .. } => 404,
    Rejection::Refused(response) => response.status().as_u16(),
    Rejection::WrongKind { .. } => 400,
  };
  TransportError::Refused {
    status,
    problem: None,
  }
}

/// Frames that also drive the forwarding of the client's items (two-way streams).
struct Forwarding {
  frames: FrameStream,
  forward: Option<BoxFuture<()>>,
}

impl futures_util::Stream for Forwarding {
  type Item = Frame;

  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Frame>> {
    let this = self.get_mut();
    if let Some(forward) = this.forward.as_mut()
      && forward.as_mut().poll(cx).is_ready()
    {
      this.forward = None;
    }
    this.frames.as_mut().poll_next(cx)
  }
}

fn forward(mut items: ItemReceiver, tx: crate::handler::InboundSender) -> BoxFuture<()> {
  Box::pin(async move {
    while let Some(item) = items.recv().await {
      if !tx.send(item).await {
        break;
      }
    }
  })
}

const WINDOW: usize = INITIAL_CREDITS as usize;

impl RpcTransport for LocalTransport {
  fn unary(&self, call: TransportCall) -> BoxFuture<Result<WireExit, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let parts = this.parts();
      let entry = dispatch::open(&this.group, &parts, &call.tag, Some(call.version))
        .await
        .map_err(rejection)?;
      let run = dispatch::start_unary(entry, call.payload, this.ctx(), CallControl::none())
        .map_err(rejection)?;
      Ok(run.await.exit)
    })
  }

  fn server_stream(&self, call: TransportCall) -> BoxFuture<Result<FrameStream, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let parts = this.parts();
      let entry = dispatch::open(&this.group, &parts, &call.tag, Some(call.version))
        .await
        .map_err(rejection)?;
      let frames = dispatch::start_server_stream(
        entry,
        this.call_id(),
        call.payload,
        this.ctx(),
        CallControl::none(),
        WINDOW,
      )
      .map_err(rejection)?;
      Ok(Box::pin(frames) as FrameStream)
    })
  }

  fn client_stream(
    &self,
    call: TransportCall,
    items: ItemReceiver,
  ) -> BoxFuture<Result<WireExit, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let parts = this.parts();
      let entry = dispatch::open(&this.group, &parts, &call.tag, Some(call.version))
        .await
        .map_err(rejection)?;
      let (tx, run) = dispatch::start_client_stream(entry, this.ctx(), CallControl::none(), WINDOW)
        .map_err(rejection)?;
      let outcome = match select(run, forward(items, tx)).await {
        Either::Left((outcome, _)) => outcome,
        Either::Right(((), run)) => run.await,
      };
      Ok(outcome.exit)
    })
  }

  fn bidi(
    &self,
    call: TransportCall,
    items: ItemReceiver,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let parts = this.parts();
      let entry = dispatch::open(&this.group, &parts, &call.tag, Some(call.version))
        .await
        .map_err(rejection)?;
      let (tx, frames) = dispatch::start_bidi(
        entry,
        this.call_id(),
        this.ctx(),
        CallControl::none(),
        WINDOW,
      )
      .map_err(rejection)?;
      Ok(Box::pin(Forwarding {
        frames: Box::pin(frames),
        forward: Some(forward(items, tx)),
      }) as FrameStream)
    })
  }

  fn subscribe(
    &self,
    call: TransportCall,
    after: Option<Cursor>,
  ) -> BoxFuture<Result<FrameStream, TransportError>> {
    let this = self.clone();
    Box::pin(async move {
      let parts = this.parts();
      let entry = dispatch::open(&this.group, &parts, &call.tag, Some(call.version))
        .await
        .map_err(rejection)?;
      let frames = dispatch::start_subscription(
        entry,
        this.call_id(),
        call.payload,
        after,
        this.ctx(),
        CallControl::none(),
        WINDOW,
      )
      .map_err(rejection)?;
      Ok(Box::pin(frames) as FrameStream)
    })
  }
}

#[cfg(feature = "client")]
pub use http_transport::{HttpHeaderError, HttpTransport};

#[cfg(feature = "client")]
mod http_transport {
  use id_effect::{Effect, Exit, HNil};
  use id_effect_axum::route::Problem;
  use id_effect_config::{Secret, SecretString};
  use id_effect_platform::http::HttpClientService;

  use super::{BoxFuture, RpcTransport, TransportCall, TransportError};
  use crate::client::http::ExtraHeaders;
  use crate::client::{RPC_HEADER, RpcClient};
  use crate::wire::{WireCall, WireExit, WireResult};

  /// Headers an [`HttpTransport`] never lets a caller set: the CSRF header every `POST /rpc`
  /// carries (`x-rpc: 1`, ADR 0013 R9), the media type, and the framing and routing headers
  /// the HTTP client owns.
  const RESERVED: [&str; 6] = [
    RPC_HEADER,
    "content-type",
    "content-length",
    "transfer-encoding",
    "host",
    "connection",
  ];

  /// Why [`HttpTransport::with_header`] or [`HttpTransport::with_bearer`] refused a header.
  /// Values never appear in the error (they may be credentials).
  #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
  #[non_exhaustive]
  pub enum HttpHeaderError {
    /// The name is not an HTTP header name (RFC 9110 token).
    #[error("invalid http header name {0:?}")]
    InvalidName(String),
    /// The value holds a byte a header value cannot (a control character such as CR or LF,
    /// or a non-ASCII byte), or a bearer token is empty.
    #[error("invalid value for http header {name:?}")]
    InvalidValue {
      /// The (lower-case) header name.
      name: String,
    },
    /// The header is the transport's own: `x-rpc` (the CSRF rule), `content-type`, or a framing
    /// header (`content-length`, `transfer-encoding`, `host`, `connection`).
    #[error("http header {0:?} is reserved by the rpc transport")]
    Reserved(String),
  }

  /// A header value's bytes: visible ASCII, space and tab (what `http::HeaderValue::from_str`
  /// accepts), so no CR, LF or NUL can split the request.
  fn valid_value(value: &str) -> bool {
    value
      .bytes()
      .all(|b| b == b'\t' || (b' '..=b'~').contains(&b))
  }

  /// The lower-case form of a header name that is valid and not reserved.
  fn header_name(name: &str) -> Result<String, HttpHeaderError> {
    let parsed = http::HeaderName::from_bytes(name.as_bytes())
      .map_err(|_| HttpHeaderError::InvalidName(name.to_owned()))?;
    let name = parsed.as_str();
    if RESERVED.contains(&name) {
      return Err(HttpHeaderError::Reserved(name.to_owned()));
    }
    Ok(name.to_owned())
  }

  /// Queries and mutations over `POST /rpc` (the [`WireCall`] envelope, `v` = the service
  /// version) through id_effect_platform's HTTP client. The calls a generated client makes in
  /// the same tick go out as one batch request, by the rules of
  /// [`RpcClient::call`](crate::client::RpcClient::call) (at most
  /// [`CLIENT_BATCH_MAX_CALLS`](crate::client::http::CLIENT_BATCH_MAX_CALLS) calls and
  /// [`CLIENT_BATCH_MAX_BYTES`](crate::client::http::CLIENT_BATCH_MAX_BYTES) bytes; a call alone
  /// is a plain envelope); [`RpcTransport::unary`] sends one at once, on its own. The stream
  /// kinds go over the WebSocket and SSE transports.
  ///
  /// [`with_header`](Self::with_header) and [`with_bearer`](Self::with_bearer) add headers to
  /// every request (validated; `x-rpc` and `content-type` stay the transport's own, so the CSRF
  /// rule holds). `Debug` shows header names only.
  ///
  /// ```
  /// use id_effect_config::Secret;
  /// use id_effect_platform::http::HttpClientService;
  /// use id_effect_rpc::client::RpcClient;
  /// use id_effect_rpc::service::{HttpHeaderError, HttpTransport};
  ///
  /// # fn transport(http: HttpClientService) -> Result<HttpTransport, HttpHeaderError> {
  /// let transport = HttpTransport::new(RpcClient::new("https://api.example.com"), http)
  ///   .with_header("x-tenant", "acme")?
  ///   .with_bearer(Secret::new("s3cr3t".to_owned()))?;
  /// assert!(!format!("{transport:?}").contains("s3cr3t"));
  /// # Ok(transport)
  /// # }
  /// ```
  #[derive(Clone)]
  pub struct HttpTransport {
    client: RpcClient,
    http: HttpClientService,
    headers: ExtraHeaders,
  }

  impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.debug_struct("HttpTransport")
        .field("url", &self.client.config().url())
        .field("headers", &self.headers.names())
        .finish_non_exhaustive()
    }
  }

  impl HttpTransport {
    /// Call the server `client` names through `http`.
    pub fn new(client: RpcClient, http: HttpClientService) -> Self {
      Self {
        client,
        http,
        headers: ExtraHeaders::default(),
      }
    }

    /// This transport with the header `name: value` on every request (replacing an earlier
    /// value of `name`; names are case-insensitive).
    ///
    /// # Errors
    ///
    /// [`HttpHeaderError::InvalidName`] when `name` is not a header name,
    /// [`HttpHeaderError::Reserved`] for `x-rpc`, `content-type` and the framing headers, and
    /// [`HttpHeaderError::InvalidValue`] when `value` holds a control character (CR, LF, …) or
    /// a non-ASCII byte.
    pub fn with_header(
      self,
      name: impl AsRef<str>,
      value: impl Into<String>,
    ) -> Result<Self, HttpHeaderError> {
      let name = header_name(name.as_ref())?;
      let value = value.into();
      if !valid_value(&value) {
        return Err(HttpHeaderError::InvalidValue { name });
      }
      let headers = self.headers.with_plain(name, value);
      Ok(Self { headers, ..self })
    }

    /// This transport with `authorization: Bearer <secret>` on every request. The token stays a
    /// [`Secret`] (zeroized on drop; `Debug` of the transport shows only the header name).
    ///
    /// # Errors
    ///
    /// [`HttpHeaderError::InvalidValue`] when the token is empty or holds a byte a header value
    /// cannot.
    pub fn with_bearer(self, secret: SecretString) -> Result<Self, HttpHeaderError> {
      let token = secret.expose();
      if token.is_empty() || !valid_value(token) {
        return Err(HttpHeaderError::InvalidValue {
          name: "authorization".to_owned(),
        });
      }
      let mut value = String::with_capacity(7 + token.len());
      value.push_str("Bearer ");
      value.push_str(token);
      let headers = self
        .headers
        .with_secret("authorization".to_owned(), Secret::new(value));
      Ok(Self { headers, ..self })
    }
  }

  pub(super) fn decode_body(status: u16, body: &[u8]) -> Result<WireExit, TransportError> {
    if let Ok(result) = serde_json::from_slice::<WireResult>(body) {
      return Ok(result.exit);
    }
    if let Ok(problem) = serde_json::from_slice::<Problem>(body) {
      return Err(TransportError::Refused {
        status: problem.status.as_u16(),
        problem: Some(Box::new(problem)),
      });
    }
    if status == 200 {
      return Err(TransportError::Protocol(
        "a 200 response that is not an rpc result".to_owned(),
      ));
    }
    // A guard's own refusal (or a proxy's) without a Problem body.
    Err(TransportError::Refused {
      status,
      problem: None,
    })
  }

  impl RpcTransport for HttpTransport {
    fn unary_effect<R: Send + Sync + 'static>(
      &self,
      call: TransportCall,
    ) -> Effect<WireExit, TransportError, R> {
      self.client.call_value(
        &self.http,
        &self.headers,
        call.tag,
        call.version,
        call.payload,
      )
    }

    fn unary(&self, call: TransportCall) -> BoxFuture<Result<WireExit, TransportError>> {
      let wire = WireCall::new(call.tag, call.payload).with_version(call.version);
      // A tag and a `serde_json::Value` always serialize.
      let body = serde_json::to_vec(&wire).unwrap_or_default();
      let req = self.headers.post(self.client.config().url(), body);
      let http = self.http.clone();
      Box::pin(async move {
        let exit = id_effect_axum::run_with_env(HNil, move |_| http.execute(req)).await;
        match exit {
          Exit::Success(resp) => decode_body(resp.status, &resp.body),
          Exit::Failure(cause) => Err(TransportError::Io(
            cause
              .fail_option()
              .map_or_else(|| "the http client failed".to_owned(), |e| e.to_string()),
          )),
        }
      })
    }
  }
}

// ---------------------------------------------------------------------------------------------
// Client calls (what the generated client methods call)
// ---------------------------------------------------------------------------------------------

fn transport_call(tag: &str, version: u32, kind: ProcedureKind, payload: Value) -> TransportCall {
  TransportCall {
    tag: tag.to_owned(),
    version,
    kind,
    payload,
  }
}

/// A query or mutation.
pub fn call_unary<T, I, O, E, R>(
  transport: &T,
  kind: ProcedureKind,
  tag: &str,
  version: u32,
  input: I,
  input_schema: WireSchema<I>,
  output: WireSchema<O>,
) -> Effect<O, RpcCallError<E>, R>
where
  T: RpcTransport,
  I: 'static,
  O: Send + 'static,
  E: RpcErrorDecode + Send + 'static,
  R: Send + Sync + 'static,
{
  let payload = match input_schema.encode(input) {
    Ok(v) => v,
    Err(why) => return fail(RpcCallError::Encode(why)),
  };
  let call = transport_call(tag, version, kind, payload);
  transport
    .unary_effect(call)
    .map_err(RpcCallError::Transport)
    .flat_map(move |exit| match exit_output(exit, &output) {
      Ok(o) => succeed(o),
      Err(e) => fail(e),
    })
}

/// The decoded items of `frames` with their cursors; the `exit` frame ends the stream or fails
/// it with the call's error.
fn typed_items<O, E, R>(
  frames: FrameStream,
  item: WireSchema<O>,
) -> Stream<(Option<Cursor>, O), RpcCallError<E>, R>
where
  O: Send + 'static,
  E: RpcErrorDecode + Send + 'static,
  R: Send + Sync + 'static,
{
  Stream::unfold_effect(Some(frames), move |state: Option<FrameStream>| {
    let item = item.clone();
    from_async(move || async move {
      let Some(mut frames) = state else {
        return Ok(None);
      };
      loop {
        let Some(frame) = frames.next().await else {
          return Err(RpcCallError::Transport(TransportError::Protocol(
            "the stream ended without an exit frame".to_owned(),
          )));
        };
        let payload = frame.payload.unwrap_or(Value::Null);
        match frame.kind {
          FrameKind::Item => {
            let value = item.decode(&payload).map_err(RpcCallError::Decode)?;
            return Ok(Some(((frame.cursor, value), Some(frames))));
          }
          FrameKind::Exit => {
            let exit: WireExit = serde_json::from_value(payload)
              .map_err(|e| RpcCallError::Transport(TransportError::Protocol(e.to_string())))?;
            return match exit {
              WireExit::Success { .. } => Ok(None),
              WireExit::Failure { error } => Err(call_error(error)),
            };
          }
          _ => {}
        }
      }
    })
  })
}

/// A server stream.
#[allow(clippy::type_complexity)]
pub fn call_server_stream<T, I, O, E, R>(
  transport: &T,
  tag: &str,
  version: u32,
  input: I,
  input_schema: WireSchema<I>,
  item: WireSchema<O>,
) -> Effect<Stream<O, RpcCallError<E>, R>, RpcCallError<E>, R>
where
  T: RpcTransport,
  I: 'static,
  O: Send + 'static,
  E: RpcErrorDecode + Send + 'static,
  R: Send + Sync + 'static,
{
  let payload = match input_schema.encode(input) {
    Ok(v) => v,
    Err(why) => return fail(RpcCallError::Encode(why)),
  };
  let call = transport_call(tag, version, ProcedureKind::ServerStream, payload);
  let transport = transport.clone();
  from_async(move || async move {
    let frames = transport.server_stream(call).await?;
    Ok(typed_items(frames, item).map_serial(|(_, value)| value))
  })
}

/// A client stream sending `items`.
pub fn call_client_stream<T, I, O, E, R>(
  transport: &T,
  tag: &str,
  version: u32,
  items: Vec<I>,
  item_schema: WireSchema<I>,
  output: WireSchema<O>,
) -> Effect<O, RpcCallError<E>, R>
where
  T: RpcTransport,
  I: 'static,
  O: Send + 'static,
  E: RpcErrorDecode + Send + 'static,
  R: Send + Sync + 'static,
{
  let (tx, rx) = mpsc::channel(items.len().max(1));
  for item in items {
    match item_schema.encode(item) {
      // The channel holds every item.
      Ok(v) => drop(tx.try_send(v)),
      Err(why) => return fail(RpcCallError::Encode(why)),
    }
  }
  drop(tx);
  let call = transport_call(tag, version, ProcedureKind::ClientStream, Value::Null);
  let transport = transport.clone();
  from_async(move || async move {
    let exit = transport.client_stream(call, rx).await?;
    exit_output(exit, &output)
  })
}

/// The client's half of a two-way stream: send items with [`BidiSender::send`], drop it to
/// end the input. Up to 32 items wait unread while the output stream is not being pulled.
pub struct BidiSender<I, E> {
  tx: mpsc::Sender<Value>,
  schema: WireSchema<I>,
  error: PhantomData<fn() -> E>,
}

impl<I, E> std::fmt::Debug for BidiSender<I, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BidiSender")
      .field("closed", &self.tx.is_closed())
      .finish()
  }
}

impl<I: 'static, E: Send + 'static> BidiSender<I, E> {
  /// Send one item.
  pub fn send<R: Send + Sync + 'static>(&self, item: I) -> Effect<(), RpcCallError<E>, R> {
    let value = match self.schema.encode(item) {
      Ok(v) => v,
      Err(why) => return fail(RpcCallError::Encode(why)),
    };
    let tx = self.tx.clone();
    from_async(move || async move {
      tx.send(value).await.map_err(|_| {
        RpcCallError::Transport(TransportError::Io(
          "the two-way stream is closed".to_owned(),
        ))
      })
    })
  }
}

/// A two-way stream: the sender of the client's items and the stream of the server's.
#[allow(clippy::type_complexity)]
pub fn call_bidi<T, I, O, E, R>(
  transport: &T,
  tag: &str,
  version: u32,
  input_schema: WireSchema<I>,
  output: WireSchema<O>,
) -> Effect<(BidiSender<I, E>, Stream<O, RpcCallError<E>, R>), RpcCallError<E>, R>
where
  T: RpcTransport,
  I: Send + 'static,
  O: Send + 'static,
  E: RpcErrorDecode + Send + 'static,
  R: Send + Sync + 'static,
{
  let call = transport_call(tag, version, ProcedureKind::Bidi, Value::Null);
  let transport = transport.clone();
  from_async(move || async move {
    let (tx, rx) = mpsc::channel(WINDOW);
    let frames = transport.bidi(call, rx).await?;
    let sender = BidiSender {
      tx,
      schema: input_schema,
      error: PhantomData,
    };
    Ok((
      sender,
      typed_items(frames, output).map_serial(|(_, value)| value),
    ))
  })
}

/// A subscription resuming after `after`; every item carries the cursor to resume after it.
#[allow(clippy::type_complexity)]
pub fn call_subscribe<T, I, O, E, R>(
  transport: &T,
  tag: &str,
  version: u32,
  input: I,
  after: Option<Cursor>,
  input_schema: WireSchema<I>,
  item: WireSchema<O>,
) -> Effect<Stream<SubscriptionItem<O>, RpcCallError<E>, R>, RpcCallError<E>, R>
where
  T: RpcTransport,
  I: 'static,
  O: Send + 'static,
  E: RpcErrorDecode + Send + 'static,
  R: Send + Sync + 'static,
{
  let payload = match input_schema.encode(input) {
    Ok(v) => v,
    Err(why) => return fail(RpcCallError::Encode(why)),
  };
  let call = transport_call(tag, version, ProcedureKind::Subscription, payload);
  let transport = transport.clone();
  from_async(move || async move {
    let frames = transport.subscribe(call, after).await?;
    Ok(typed_items(frames, item).map_serial(|(cursor, value)| SubscriptionItem { cursor, value }))
  })
}

// ---------------------------------------------------------------------------------------------
// Macro support
// ---------------------------------------------------------------------------------------------

/// Paths the macros expand to. Not a public API.
#[doc(hidden)]
pub mod __private {
  pub use http::StatusCode;
  pub use id_effect::{DynEnv, Effect, Stream, die};
  pub use id_effect_axum::route::Router;
  pub use serde_json::{Map, Value};

  use serde_json::json;

  use super::RpcServiceError;
  use crate::handler::WireSchema;

  /// The status `code` (validated by the macro).
  pub fn status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST)
  }

  /// A field's wire value (`null` when it does not encode, which the server answers as a
  /// fault only if the whole union is then malformed).
  pub fn encode_field<T: 'static>(schema: &WireSchema<T>, value: T) -> Value {
    schema.encode(value).unwrap_or(Value::Null)
  }

  /// Field `name` of an error value (a missing field decodes from `null`).
  pub fn decode_field<T: 'static>(error: &Value, name: &str, schema: &WireSchema<T>) -> Option<T> {
    schema.decode(error.get(name).unwrap_or(&Value::Null)).ok()
  }

  /// The `_tag` of an error value.
  pub fn tag_of(value: &Value) -> Option<&str> {
    value.get("_tag").and_then(Value::as_str)
  }

  /// One member of an error union: `{"_tag": tag, …fields}`.
  pub fn variant_schema(tag: &str, fields: Vec<(&str, Value, bool)>) -> Value {
    let mut properties = Map::new();
    properties.insert("_tag".to_owned(), json!({"const": tag}));
    let mut required = vec![Value::from("_tag")];
    let mut defs = Map::new();
    for (name, schema, is_required) in fields {
      properties.insert(name.to_owned(), super::hoist_defs(schema, &mut defs));
      if is_required {
        required.push(Value::from(name));
      }
    }
    super::with_defs(
      json!({"type": "object", "properties": properties, "required": required}),
      defs,
    )
  }

  /// The union of `variants` (`None` when every variant is internal).
  pub fn union_schema(variants: Vec<Value>) -> Option<Value> {
    if variants.is_empty() {
      return None;
    }
    let mut defs = Map::new();
    let variants: Vec<Value> = variants
      .into_iter()
      .map(|v| super::hoist_defs(v, &mut defs))
      .collect();
    Some(super::with_defs(json!({"oneOf": variants}), defs))
  }

  /// A schema whose type may or may not have serde: `(&SerdeProbe(s)).wire_schema()` is
  /// `s.with_serde()` ([`WithSerde`]) when `T: Serialize + DeserializeOwned`, else `s`
  /// ([`WithoutSerde`]); method resolution tries the by-reference impl first.
  pub struct SerdeProbe<T>(pub WireSchema<T>);

  /// The serde case of [`SerdeProbe`]: the procedure is served over postcard too.
  pub trait WithSerde<T> {
    /// The schema with its typed codec.
    fn wire_schema(&self) -> WireSchema<T>;
  }

  impl<T> WithSerde<T> for SerdeProbe<T>
  where
    T: serde::Serialize + serde::de::DeserializeOwned + 'static,
  {
    fn wire_schema(&self) -> WireSchema<T> {
      self.0.clone().with_serde()
    }
  }

  /// The fallback of [`SerdeProbe`]: JSON only.
  pub trait WithoutSerde<T> {
    /// The schema as it is.
    fn wire_schema(&self) -> WireSchema<T>;
  }

  impl<T: 'static> WithoutSerde<T> for &SerdeProbe<T> {
    fn wire_schema(&self) -> WireSchema<T> {
      self.0.clone()
    }
  }

  /// Checks that `E` is an error union.
  pub fn assert_error<E: RpcServiceError>() {}
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn built_in_types_round_trip_and_describe_themselves() {
    assert_eq!(types::string().decode(&json!("a")).unwrap(), "a");
    assert_eq!(types::i64().encode(3).unwrap(), json!(3));
    assert_eq!(types::f64().decode(&json!(1.5)).unwrap(), 1.5);
    assert!(types::bool().decode(&json!(true)).unwrap());
    assert!(types::unit().decode(&Value::Null).is_ok());
    assert_eq!(
      types::unit().decode(&json!(1)).unwrap_err()[0].message,
      "expected null"
    );
    assert_eq!(types::unit().encode(()).unwrap(), Value::Null);
    assert_eq!(types::unit().json_schema()["type"], "null");

    let opt = types::option(types::i64());
    assert_eq!(opt.decode(&Value::Null).unwrap(), None);
    assert_eq!(opt.decode(&json!(2)).unwrap(), Some(2));
    assert_eq!(opt.encode(None).unwrap(), Value::Null);
    assert_eq!(opt.encode(Some(4)).unwrap(), json!(4));
    assert_eq!(opt.json_schema()["anyOf"][1], json!({"type": "null"}));

    let list = types::vec(types::string());
    assert_eq!(list.decode(&json!(["a", "b"])).unwrap(), ["a", "b"]);
    assert_eq!(list.encode(vec!["x".into()]).unwrap(), json!(["x"]));
    assert_eq!(
      list.decode(&json!(1)).unwrap_err()[0].message,
      "expected an array"
    );
    let issues = list.decode(&json!(["a", 1, 2])).unwrap_err();
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].path, "/1");
    assert_eq!(list.json_schema()["type"], "array");
    let failing = types::vec(WireSchema::<i64>::from_fns(
      |_| Ok(1),
      |_| Err("no".to_owned()),
      json!({}),
    ));
    assert!(failing.encode(vec![1]).is_err());
  }

  #[test]
  fn composites_hoist_defs_to_their_root() {
    let inner = WireSchema::<i64>::from_fns(
      |_| Ok(1),
      |_| Ok(json!(1)),
      json!({"$schema": "x", "$ref": "#/$defs/A", "$defs": {"A": {"type": "integer"}}}),
    );
    let opt = types::option(inner.clone());
    assert_eq!(
      opt.json_schema(),
      &json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "anyOf": [{"$ref": "#/$defs/A"}, {"type": "null"}],
        "$defs": {"A": {"type": "integer"}}
      })
    );
    assert_eq!(
      types::vec(inner).json_schema()["$defs"]["A"]["type"],
      "integer"
    );
    let variant = __private::variant_schema("V", vec![("a", opt.json_schema().clone(), true)]);
    assert!(variant["$defs"]["A"].is_object());
    assert!(variant["properties"]["a"].get("$defs").is_none());
    let union = __private::union_schema(vec![variant]).unwrap();
    assert!(union["$defs"]["A"].is_object());
    assert!(union["oneOf"][0].get("$defs").is_none());
    assert_eq!(
      hoist_defs(json!(true), &mut serde_json::Map::new()),
      json!(true)
    );
    let defs = [("A".to_owned(), json!(1))].into_iter().collect();
    assert_eq!(with_defs(json!(true), defs), json!(true));
  }

  #[test]
  fn labs_rpc_error_and_never_decode() {
    for err in [
      RpcError::invalid_argument("a"),
      RpcError::not_found("b"),
      RpcError::already_exists("c"),
      RpcError::unauthenticated("d"),
    ] {
      let back = RpcError::from_wire(&err.to_wire()).expect("a contract variant");
      assert_eq!(back.to_wire(), err.to_wire());
    }
    assert!(RpcError::from_wire(&RpcError::internal("x").to_wire()).is_none());
    assert!(RpcError::from_wire(&json!({"_tag": "NotFound"})).is_none());
    assert!(Never::from_wire(&json!({"_tag": "A"})).is_none());
  }

  #[test]
  fn call_errors_sort_reserved_typed_and_unknown_failures() {
    let reserved: RpcCallError<RpcError> = call_error(json!({"_tag": "DeadlineExceeded"}));
    assert_eq!(reserved.failure(), Some(&RpcFailure::DeadlineExceeded));
    assert!(reserved.typed().is_none());
    let malformed: RpcCallError<RpcError> = call_error(json!({"_tag": "InternalError"}));
    assert!(matches!(malformed, RpcCallError::Unexpected(_)));
    let typed: RpcCallError<RpcError> = call_error(json!({"_tag": "NotFound", "message": "m"}));
    assert!(typed.typed().is_some());
    assert!(typed.failure().is_none());
    let other: RpcCallError<RpcError> = call_error(json!({"_tag": "Newer"}));
    assert!(other.to_string().contains("Newer"));
    let out: Result<i64, RpcCallError<Never>> =
      exit_output(WireExit::Success { value: json!("x") }, &types::i64());
    assert!(matches!(out, Err(RpcCallError::Decode(ref i)) if i.len() == 1));
    for e in [
      RpcCallError::<Never>::Encode("e".into()),
      RpcCallError::Transport(TransportError::Unsupported(ProcedureKind::Bidi)),
      RpcCallError::Transport(TransportError::Io("down".into())),
      RpcCallError::Transport(TransportError::Protocol("p".into())),
      RpcCallError::Transport(TransportError::Refused {
        status: 403,
        problem: None,
      }),
      RpcCallError::Typed(RpcError::not_found("x"))
        .typed()
        .map(|_| RpcCallError::Decode(vec![]))
        .unwrap(),
      RpcCallError::Failure(RpcFailure::Cancelled),
    ] {
      assert!(!e.to_string().is_empty());
    }
  }

  #[test]
  fn private_helpers_build_union_members() {
    use __private::*;
    assert_eq!(status(404), StatusCode::NOT_FOUND);
    assert_eq!(status(42), StatusCode::BAD_REQUEST);
    assert_eq!(encode_field(&types::i64(), 3), json!(3));
    let bad = WireSchema::<i64>::from_fns(|_| Ok(1), |_| Err("no".into()), json!({}));
    assert_eq!(encode_field(&bad, 1), Value::Null);
    let value = json!({"_tag": "A", "n": 2});
    assert_eq!(decode_field(&value, "n", &types::i64()), Some(2));
    assert_eq!(
      decode_field(&value, "m", &types::option(types::i64())),
      Some(None)
    );
    assert_eq!(tag_of(&value), Some("A"));
    let schema = variant_schema(
      "A",
      vec![
        ("n", json!({"type": "integer"}), true),
        ("m", json!({}), false),
      ],
    );
    assert_eq!(schema["required"], json!(["_tag", "n"]));
    assert_eq!(schema["properties"]["_tag"], json!({"const": "A"}));
    assert!(union_schema(vec![]).is_none());
    assert_eq!(union_schema(vec![json!(1)]).unwrap(), json!({"oneOf": [1]}));
    assert_error::<Never>();
  }

  #[tokio::test]
  async fn transports_default_to_unsupported_kinds() {
    #[derive(Clone)]
    struct UnaryOnly;
    impl RpcTransport for UnaryOnly {
      fn unary(&self, _call: TransportCall) -> BoxFuture<Result<WireExit, TransportError>> {
        Box::pin(async { Ok(WireExit::Success { value: Value::Null }) })
      }
    }
    let call = |kind| transport_call("A.b", 1, kind, Value::Null);
    let t = UnaryOnly;
    assert!(t.unary(call(ProcedureKind::Query)).await.is_ok());
    let (_tx, rx) = mpsc::channel(1);
    assert!(matches!(
      t.client_stream(call(ProcedureKind::ClientStream), rx).await,
      Err(TransportError::Unsupported(ProcedureKind::ClientStream))
    ));
    assert!(
      t.server_stream(call(ProcedureKind::ServerStream))
        .await
        .is_err()
    );
    let (_tx, rx) = mpsc::channel(1);
    assert!(t.bidi(call(ProcedureKind::Bidi), rx).await.is_err());
    assert!(
      t.subscribe(call(ProcedureKind::Subscription), None)
        .await
        .is_err()
    );
    let local = LocalTransport::new(RpcGroup::new(), DynEnv::new());
    assert!(format!("{local:?}").contains("procedures"));
  }

  #[cfg(feature = "client")]
  #[test]
  fn http_bodies_decode_as_results_problems_or_protocol_errors() {
    use super::http_transport::decode_body;
    let ok = decode_body(200, br#"{"tag":"A.b","exit":{"_tag":"Success","value":1}}"#).unwrap();
    assert_eq!(ok, WireExit::Success { value: json!(1) });
    let refused = decode_body(404, br#"{"status":404,"title":"Not Found"}"#).unwrap_err();
    assert!(matches!(
      refused,
      TransportError::Refused {
        status: 404,
        problem: Some(_)
      }
    ));
    let other = decode_body(502, b"gateway").unwrap_err();
    assert!(matches!(
      other,
      TransportError::Refused {
        status: 502,
        problem: None
      }
    ));
    let garbage = decode_body(200, b"gateway").unwrap_err();
    assert!(matches!(garbage, TransportError::Protocol(_)));
  }
}
