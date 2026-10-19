//! The procedure registry: an [`RpcGroup`] of tagged procedures, each with a kind, a version
//! and an access decision (ADR 0014 C1, the H8 rule of typed routes applied per procedure).
//!
//! A [`Procedure`] starts [`Undecided`]; [`Procedure::public`] or [`Procedure::guard`] decides
//! who may call it, and every registration method accepts only a decided procedure, so
//! forgetting the decision is a compile error rather than an open procedure. There is one
//! registration method per kind, each taking the handler shape of [`crate::handler`]:
//!
//! | Kind | Procedure | Registration |
//! |---|---|---|
//! | query | [`Procedure::query`] | [`RpcGroup::query`] (or the schema-tuple [`RpcGroup::register`]) |
//! | mutation | [`Procedure::mutation`] | [`RpcGroup::mutation`] (or [`RpcGroup::register`]) |
//! | server stream | [`Procedure::server_stream`] | [`RpcGroup::server_stream`] |
//! | client stream | [`Procedure::client_stream`] | [`RpcGroup::client_stream`] |
//! | bidi | [`Procedure::bidi`] | [`RpcGroup::bidi`] |
//! | subscription | [`Procedure::subscription`] | [`RpcGroup::subscription`] |
//!
//! ```rust
//! use std::sync::Arc;
//! use id_effect::{Never, schema, succeed};
//! use id_effect_rpc::{Procedure, RpcGroup, WireSchema};
//!
//! let mut group = RpcGroup::new();
//! group.register(
//!   Procedure::query("Greeter.greet").public(),
//!   Arc::new(schema::struct_("name", schema::string::<()>(), "times", schema::i64::<()>())),
//!   Arc::new(schema::struct_("message", schema::string::<()>(), "times", schema::i64::<()>())),
//!   &["message", "times"],
//!   |(name, times), _env| succeed((format!("Hello, {name}!"), times)),
//! );
//! group.mutation(
//!   Procedure::mutation("Greeter.shout").public(),
//!   WireSchema::new(schema::string::<()>()),
//!   WireSchema::new(schema::string::<()>()),
//!   |text: String, _env: &mut id_effect::DynEnv| succeed::<_, Never, _>(text.to_uppercase()),
//! );
//! assert_eq!(group.tags().collect::<Vec<_>>(), ["Greeter.greet", "Greeter.shout"]);
//! ```
//!
//! ```compile_fail
//! use std::sync::Arc;
//! use id_effect::{schema, succeed};
//! use id_effect_rpc::{Procedure, RpcGroup};
//!
//! let mut group = RpcGroup::new();
//! group.register(
//!   Procedure::query("Greeter.greet"), // neither `.public()` nor `.guard(g)`
//!   Arc::new(schema::string::<()>()),
//!   Arc::new(schema::string::<()>()),
//!   &["message"],
//!   |name: String, _env| succeed(name),
//! );
//! ```
//!
//! Guards are `id_effect_axum`'s [`RouteGuard`]s (`id_effect_authz`'s `Guard` implements it):
//! the dispatcher runs them on the request head, outermost (last added) first, before the
//! payload is decoded. Versions are served side by side: a call names one, or gets the newest.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use id_effect::schema::Schema;
use id_effect::schema::data::EffectData;
use id_effect::{DynEnv, Effect};
use id_effect_axum::route::RouteAccess;
pub use id_effect_axum::route::{Access, Decided, Guarded, Public, RouteGuard, Undecided};
use serde_json::Value;

use crate::RpcError;
use crate::codec::ProcedureCodecs;
use crate::contract::{ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract};
use crate::error_union::RpcErrorUnion;
use crate::handler::{
  BidiHandler, ClientStreamHandler, ErasedHandler, MutationHandler, QueryHandler,
  ServerStreamHandler, SubscriptionHandler, WireSchema, decode_with, erase_bidi,
  erase_client_stream, erase_server_stream, erase_subscription, erase_unary,
};
use crate::serialization::StructWireJson;

/// The longest procedure tag.
pub const MAX_TAG_LEN: usize = 128;

/// Default cap on the items of one client stream call.
pub const DEFAULT_MAX_STREAM_ITEMS: u64 = 10_000;

/// Default cap on the bytes of one client stream call (4 MiB).
pub const DEFAULT_MAX_STREAM_BYTES: u64 = 4 * 1024 * 1024;

/// Whether `tag` is a valid procedure tag: 1 to [`MAX_TAG_LEN`] characters of
/// `[A-Za-z0-9_.-]` (`"<Service>.<procedure>"` by convention).
pub fn is_valid_tag(tag: &str) -> bool {
  !tag.is_empty()
    && tag.len() <= MAX_TAG_LEN
    && tag
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// One procedure before registration: its tag, kind, version, limits and access decision.
///
/// `A` is the access state: [`Undecided`] until [`Procedure::public`] or [`Procedure::guard`],
/// then [`Public`] or [`Guarded`]; registration only accepts a decided procedure.
#[must_use = "a procedure does nothing until it is registered in an `RpcGroup`"]
pub struct Procedure<A = Undecided> {
  tag: String,
  kind: ProcedureKind,
  version: u32,
  description: Option<String>,
  max_items: Option<u64>,
  max_bytes: Option<u64>,
  guards: Vec<Arc<dyn RouteGuard>>,
  access: PhantomData<A>,
}

impl Procedure<Undecided> {
  fn new(tag: impl Into<String>, kind: ProcedureKind) -> Self {
    let tag = tag.into();
    assert!(
      is_valid_tag(&tag),
      "invalid rpc procedure tag {tag:?}: use 1 to {MAX_TAG_LEN} characters of [A-Za-z0-9_.-]"
    );
    Self {
      tag,
      kind,
      version: 1,
      description: None,
      max_items: None,
      max_bytes: None,
      guards: Vec::new(),
      access: PhantomData,
    }
  }

  /// A retry-safe, read-only procedure.
  ///
  /// # Panics
  ///
  /// When `tag` is not a valid tag ([`is_valid_tag`]); so do the other constructors.
  pub fn query(tag: impl Into<String>) -> Self {
    Self::new(tag, ProcedureKind::Query)
  }

  /// A state-changing procedure.
  pub fn mutation(tag: impl Into<String>) -> Self {
    Self::new(tag, ProcedureKind::Mutation)
  }

  /// One input, a stream of outputs.
  pub fn server_stream(tag: impl Into<String>) -> Self {
    Self::new(tag, ProcedureKind::ServerStream)
  }

  /// A stream of inputs, one output.
  pub fn client_stream(tag: impl Into<String>) -> Self {
    Self::new(tag, ProcedureKind::ClientStream)
  }

  /// A two-way stream.
  pub fn bidi(tag: impl Into<String>) -> Self {
    Self::new(tag, ProcedureKind::Bidi)
  }

  /// A resumable stream of items from a cursor.
  pub fn subscription(tag: impl Into<String>) -> Self {
    Self::new(tag, ProcedureKind::Subscription)
  }

  /// Anyone may call this procedure, anonymous callers included.
  pub fn public(self) -> Procedure<Public> {
    self.into_state()
  }
}

impl<A> Procedure<A> {
  /// Run `guard` before the handler (repeatable; each later guard wraps the earlier ones,
  /// outermost last). On a public procedure it makes the procedure guarded.
  pub fn guard(mut self, guard: impl RouteGuard) -> Procedure<Guarded> {
    self.guards.push(Arc::new(guard));
    self.into_state()
  }

  /// Describe the procedure (recorded in the contract).
  pub fn describe(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }

  /// The service contract version this procedure belongs to (default 1).
  ///
  /// # Panics
  ///
  /// When `version` is 0.
  pub fn version(mut self, version: u32) -> Self {
    assert!(version >= 1, "rpc service versions start at 1");
    self.version = version;
    self
  }

  /// Client streams: accept at most `items` input items (default [`DEFAULT_MAX_STREAM_ITEMS`]).
  pub fn max_items(mut self, items: u64) -> Self {
    self.max_items = Some(items);
    self
  }

  /// Client streams: accept at most `bytes` of input (default [`DEFAULT_MAX_STREAM_BYTES`]).
  pub fn max_bytes(mut self, bytes: u64) -> Self {
    self.max_bytes = Some(bytes);
    self
  }

  /// The tag.
  pub fn tag(&self) -> &str {
    &self.tag
  }

  /// The kind.
  pub fn kind(&self) -> ProcedureKind {
    self.kind
  }

  fn into_state<B>(self) -> Procedure<B> {
    Procedure {
      tag: self.tag,
      kind: self.kind,
      version: self.version,
      description: self.description,
      max_items: self.max_items,
      max_bytes: self.max_bytes,
      guards: self.guards,
      access: PhantomData,
    }
  }
}

impl<A: Access> Procedure<A> {
  /// The access state as [`RouteAccess`] (`Undecided`, `Public` or `Guarded`).
  pub fn route_access(&self) -> RouteAccess {
    A::ACCESS
  }
}

impl<A> fmt::Debug for Procedure<A> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Procedure")
      .field("tag", &self.tag)
      .field("kind", &self.kind)
      .field("version", &self.version)
      .field(
        "policies",
        &self.guards.iter().map(|g| g.policy()).collect::<Vec<_>>(),
      )
      .finish()
  }
}

/// One registered procedure: tag, kind, version, access decision, guards, schemas, limits and
/// the erased handler.
#[derive(Clone)]
pub struct RpcMethodEntry {
  /// Stable procedure tag.
  pub tag: String,
  kind: ProcedureKind,
  version: u32,
  access: ProcedureAccess,
  guards: Vec<Arc<dyn RouteGuard>>,
  input: Value,
  output: Value,
  error: Option<Value>,
  description: Option<String>,
  max_items: u64,
  max_bytes: u64,
  limits_declared: bool,
  handler: ErasedHandler,
  codecs: ProcedureCodecs,
}

impl RpcMethodEntry {
  /// Procedure tag.
  #[inline]
  pub fn tag(&self) -> &str {
    &self.tag
  }

  /// Procedure kind.
  pub fn kind(&self) -> ProcedureKind {
    self.kind
  }

  /// Service contract version.
  pub fn version(&self) -> u32 {
    self.version
  }

  /// The access decision recorded in the contract.
  pub fn access(&self) -> &ProcedureAccess {
    &self.access
  }

  /// The guards, outermost last (empty for a public procedure).
  pub fn guards(&self) -> &[Arc<dyn RouteGuard>] {
    &self.guards
  }

  /// Client streams: the item cap.
  pub fn max_items(&self) -> u64 {
    self.max_items
  }

  /// Client streams: the byte cap.
  pub fn max_bytes(&self) -> u64 {
    self.max_bytes
  }

  /// The erased handler every transport drives (see [`crate::server::dispatch`]).
  pub fn handler(&self) -> &ErasedHandler {
    &self.handler
  }

  /// The typed codec bridges of the input and output schemas (empty unless both were built
  /// with [`WireSchema::with_serde`]): what a transport uses for a codec that is not
  /// self-describing, such as postcard.
  pub fn codecs(&self) -> &ProcedureCodecs {
    &self.codecs
  }

  /// The procedure's contract entry, named `name`.
  pub fn contract(&self, name: impl Into<String>) -> ProcedureContract {
    let mut procedure = ProcedureContract::new(
      name,
      self.kind,
      self.access.clone(),
      self.input.clone(),
      self.output.clone(),
    );
    procedure.error = self.error.clone();
    procedure.description = self.description.clone();
    procedure.idempotent = self.kind == ProcedureKind::Mutation;
    if self.kind == ProcedureKind::ClientStream || self.limits_declared {
      procedure.max_items = Some(self.max_items);
      procedure.max_bytes = Some(self.max_bytes);
    }
    procedure
  }
}

impl fmt::Debug for RpcMethodEntry {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RpcMethodEntry")
      .field("tag", &self.tag)
      .field("kind", &self.kind)
      .field("version", &self.version)
      .field("access", &self.access)
      .field("handler", &self.handler)
      .finish_non_exhaustive()
  }
}

/// Registered procedures keyed by `(tag, version)`
/// ([`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc) `RpcGroup` parity), every one
/// with an access decision.
#[derive(Clone, Default)]
pub struct RpcGroup {
  methods: BTreeMap<(String, u32), RpcMethodEntry>,
}

impl RpcGroup {
  /// Empty group.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  #[allow(clippy::too_many_arguments)]
  fn insert<D: Decided>(
    &mut self,
    procedure: Procedure<D>,
    allowed: &[ProcedureKind],
    method: &str,
    input: Value,
    output: Value,
    error: Option<Value>,
    handler: impl FnOnce(Arc<str>) -> ErasedHandler,
  ) {
    let Procedure {
      tag,
      kind,
      version,
      description,
      max_items,
      max_bytes,
      guards,
      ..
    } = procedure;
    assert!(
      allowed.contains(&kind),
      "rpc procedure {tag:?} is a {kind}; `RpcGroup::{method}` registers {}",
      allowed
        .iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(" or ")
    );
    let key = (tag.clone(), version);
    assert!(
      !self.methods.contains_key(&key),
      "rpc procedure {tag:?} version {version} is registered twice"
    );
    let access = match D::ACCESS {
      RouteAccess::Guarded => ProcedureAccess::Guarded {
        policies: guards.iter().map(|g| g.policy().to_owned()).collect(),
      },
      _ => ProcedureAccess::Public {},
    };
    let handler = handler(Arc::from(tag.as_str()));
    self.methods.insert(
      key,
      RpcMethodEntry {
        tag,
        kind,
        version,
        access,
        guards,
        input,
        output,
        error,
        description,
        limits_declared: max_items.is_some() || max_bytes.is_some(),
        max_items: max_items.unwrap_or(DEFAULT_MAX_STREAM_ITEMS),
        max_bytes: max_bytes.unwrap_or(DEFAULT_MAX_STREAM_BYTES),
        handler,
        codecs: ProcedureCodecs::default(),
      },
    );
  }

  /// The registry key of `procedure` and the typed bridges of its two schemas.
  fn typed_codecs<D, I: 'static, O: 'static>(
    procedure: &Procedure<D>,
    input: &WireSchema<I>,
    output: &WireSchema<O>,
  ) -> ((String, u32), ProcedureCodecs) {
    let codecs = ProcedureCodecs::new(
      procedure.kind,
      input.typed_codec().cloned(),
      output.typed_codec().cloned(),
    );
    ((procedure.tag.clone(), procedure.version), codecs)
  }

  /// Record the typed bridges of the entry just inserted at `key`.
  fn attach_codecs(&mut self, key: (String, u32), codecs: ProcedureCodecs) {
    if let Some(entry) = self.methods.get_mut(&key) {
      entry.codecs = codecs;
    }
  }

  /// Register a query or mutation from tuple Schemas, with the labs [`RpcError`] as its error
  /// union; `output_fields` names the members of the encoded output object.
  ///
  /// Each call runs the handler's effect as one request fiber. A typed [`RpcError`] is a
  /// `Failure` (its status), a defect or interrupt is `InternalError { id }` (the message is
  /// logged, never sent).
  ///
  /// # Panics
  ///
  /// When the procedure is not a query or mutation, or `(tag, version)` is already registered.
  pub fn register<D, A, IA, EA, B, IB, EB, F>(
    &mut self,
    procedure: Procedure<D>,
    input: Arc<Schema<A, IA, EA>>,
    output: Arc<Schema<B, IB, EB>>,
    output_fields: &'static [&'static str],
    handler: F,
  ) where
    D: Decided,
    A: Send + 'static,
    B: Send + 'static,
    EA: EffectData + 'static,
    EB: EffectData + 'static,
    IA: Send + Sync + 'static,
    IB: Send + Sync + 'static + StructWireJson,
    F: Fn(A, &mut DynEnv) -> Effect<B, RpcError, DynEnv> + Send + Sync + 'static,
  {
    let input_schema = input.json_schema();
    let input = WireSchema::from_fns(
      move |v| decode_with(&input, v),
      |_| Err("a procedure input is never encoded by the server".to_owned()),
      input_schema.clone(),
    );
    let output = WireSchema::with_fields((*output).clone(), output_fields);
    let output_schema = output.json_schema().clone();
    self.insert(
      procedure,
      &[ProcedureKind::Query, ProcedureKind::Mutation],
      "register",
      input_schema,
      output_schema,
      RpcError::union_schema(),
      move |tag| ErasedHandler::Unary(erase_unary(tag, input, output, handler)),
    );
  }

  /// Register a query.
  ///
  /// # Panics
  ///
  /// When the procedure is not a [query](Procedure::query), or `(tag, version)` is already
  /// registered; the other registration methods panic likewise for their kind.
  pub fn query<D, I, O, E, H>(
    &mut self,
    procedure: Procedure<D>,
    input: WireSchema<I>,
    output: WireSchema<O>,
    handler: H,
  ) where
    D: Decided,
    I: Send + 'static,
    O: Send + 'static,
    E: RpcErrorUnion,
    H: QueryHandler<I, O, E>,
  {
    let schemas = (input.json_schema().clone(), output.json_schema().clone());
    let (key, codecs) = Self::typed_codecs(&procedure, &input, &output);
    self.insert(
      procedure,
      &[ProcedureKind::Query],
      "query",
      schemas.0,
      schemas.1,
      E::union_schema(),
      move |tag| {
        ErasedHandler::Unary(erase_unary(
          tag,
          input,
          output,
          move |i, env: &mut DynEnv| handler.call(i, env),
        ))
      },
    );
    self.attach_codecs(key, codecs);
  }

  /// Register a mutation.
  pub fn mutation<D, I, O, E, H>(
    &mut self,
    procedure: Procedure<D>,
    input: WireSchema<I>,
    output: WireSchema<O>,
    handler: H,
  ) where
    D: Decided,
    I: Send + 'static,
    O: Send + 'static,
    E: RpcErrorUnion,
    H: MutationHandler<I, O, E>,
  {
    let schemas = (input.json_schema().clone(), output.json_schema().clone());
    let (key, codecs) = Self::typed_codecs(&procedure, &input, &output);
    self.insert(
      procedure,
      &[ProcedureKind::Mutation],
      "mutation",
      schemas.0,
      schemas.1,
      E::union_schema(),
      move |tag| {
        ErasedHandler::Unary(erase_unary(
          tag,
          input,
          output,
          move |i, env: &mut DynEnv| handler.call(i, env),
        ))
      },
    );
    self.attach_codecs(key, codecs);
  }

  /// Register a server stream (`item` describes one output item).
  pub fn server_stream<D, I, O, E, H>(
    &mut self,
    procedure: Procedure<D>,
    input: WireSchema<I>,
    item: WireSchema<O>,
    handler: H,
  ) where
    D: Decided,
    I: Send + 'static,
    O: Send + 'static,
    E: RpcErrorUnion,
    H: ServerStreamHandler<I, O, E>,
  {
    let schemas = (input.json_schema().clone(), item.json_schema().clone());
    let (key, codecs) = Self::typed_codecs(&procedure, &input, &item);
    self.insert(
      procedure,
      &[ProcedureKind::ServerStream],
      "server_stream",
      schemas.0,
      schemas.1,
      E::union_schema(),
      move |tag| ErasedHandler::ServerStream(erase_server_stream(tag, input, item, handler)),
    );
    self.attach_codecs(key, codecs);
  }

  /// Register a client stream (`item` describes one input item).
  pub fn client_stream<D, I, O, E, H>(
    &mut self,
    procedure: Procedure<D>,
    item: WireSchema<I>,
    output: WireSchema<O>,
    handler: H,
  ) where
    D: Decided,
    I: Send + 'static,
    O: Send + 'static,
    E: RpcErrorUnion,
    H: ClientStreamHandler<I, O, E>,
  {
    let schemas = (item.json_schema().clone(), output.json_schema().clone());
    let (key, codecs) = Self::typed_codecs(&procedure, &item, &output);
    self.insert(
      procedure,
      &[ProcedureKind::ClientStream],
      "client_stream",
      schemas.0,
      schemas.1,
      E::union_schema(),
      move |tag| ErasedHandler::ClientStream(erase_client_stream(tag, item, output, handler)),
    );
    self.attach_codecs(key, codecs);
  }

  /// Register a two-way stream (`input` and `output` describe one item each way).
  pub fn bidi<D, I, O, E, H>(
    &mut self,
    procedure: Procedure<D>,
    input: WireSchema<I>,
    output: WireSchema<O>,
    handler: H,
  ) where
    D: Decided,
    I: Send + 'static,
    O: Send + 'static,
    E: RpcErrorUnion,
    H: BidiHandler<I, O, E>,
  {
    let schemas = (input.json_schema().clone(), output.json_schema().clone());
    let (key, codecs) = Self::typed_codecs(&procedure, &input, &output);
    self.insert(
      procedure,
      &[ProcedureKind::Bidi],
      "bidi",
      schemas.0,
      schemas.1,
      E::union_schema(),
      move |tag| ErasedHandler::Bidi(erase_bidi(tag, input, output, handler)),
    );
    self.attach_codecs(key, codecs);
  }

  /// Register a subscription (`item` describes one item).
  pub fn subscription<D, I, O, E, H>(
    &mut self,
    procedure: Procedure<D>,
    input: WireSchema<I>,
    item: WireSchema<O>,
    handler: H,
  ) where
    D: Decided,
    I: Send + 'static,
    O: Send + 'static,
    E: RpcErrorUnion,
    H: SubscriptionHandler<I, O, E>,
  {
    let schemas = (input.json_schema().clone(), item.json_schema().clone());
    let (key, codecs) = Self::typed_codecs(&procedure, &input, &item);
    self.insert(
      procedure,
      &[ProcedureKind::Subscription],
      "subscription",
      schemas.0,
      schemas.1,
      E::union_schema(),
      move |tag| ErasedHandler::Subscription(erase_subscription(tag, input, item, handler)),
    );
    self.attach_codecs(key, codecs);
  }

  /// The newest version of the procedure `tag`.
  pub fn get(&self, tag: &str) -> Option<&RpcMethodEntry> {
    self
      .methods
      .range((tag.to_owned(), 0)..=(tag.to_owned(), u32::MAX))
      .next_back()
      .map(|(_, entry)| entry)
  }

  /// Version `version` of the procedure `tag`.
  pub fn get_version(&self, tag: &str, version: u32) -> Option<&RpcMethodEntry> {
    self.methods.get(&(tag.to_owned(), version))
  }

  /// Every registered tag once, sorted.
  pub fn tags(&self) -> impl Iterator<Item = &str> {
    let mut tags: Vec<&str> = self.methods.keys().map(|(tag, _)| tag.as_str()).collect();
    tags.dedup();
    tags.into_iter()
  }

  /// Every registered procedure, sorted by tag, then version.
  pub fn entries(&self) -> impl Iterator<Item = &RpcMethodEntry> {
    self.methods.values()
  }

  /// Number of registered procedures (each version counts).
  #[inline]
  pub fn len(&self) -> usize {
    self.methods.len()
  }

  /// Whether the group has no procedures.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.methods.is_empty()
  }

  /// The contract of service `name` at `version`: every procedure of that version whose tag
  /// starts with `"<name>."`, named by the rest of its tag.
  pub fn service_contract(&self, name: &str, version: u32) -> ServiceContract {
    let prefix = format!("{name}.");
    self
      .entries()
      .filter(|entry| entry.version == version)
      .filter_map(|entry| {
        entry
          .tag
          .strip_prefix(&prefix)
          .map(|procedure| entry.contract(procedure))
      })
      .fold(ServiceContract::new(name, version), |service, procedure| {
        service.with_procedure(procedure)
      })
  }
}

impl fmt::Debug for RpcGroup {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_list().entries(self.methods.values()).finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::handler::{CallContext, CallOutcome};
  use crate::wire::{RpcFailure, WireExit};
  use axum::http::request::Parts;
  use id_effect::schema;
  use id_effect::{Never, die, succeed};
  use id_effect_axum::route::GuardFuture;

  struct Staff;

  impl RouteGuard for Staff {
    fn policy(&self) -> &str {
      "staff"
    }

    fn check<'a>(&'a self, _parts: &'a Parts) -> GuardFuture<'a> {
      Box::pin(async { Ok(()) })
    }
  }

  fn pair() -> Arc<Schema<(String, i64), (String, i64)>> {
    Arc::new(schema::struct_(
      "name",
      schema::string::<()>(),
      "n",
      schema::i64::<()>(),
    ))
  }

  async fn call(group: &RpcGroup, tag: &str, payload: Value) -> CallOutcome {
    let ErasedHandler::Unary(f) = group.get(tag).expect("registered").handler() else {
      panic!("not unary");
    };
    f(payload, CallContext::new(DynEnv::new(), "cid-1")).await
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn register_and_call_greet() {
    let mut group = RpcGroup::new();
    group.register(
      Procedure::query("Greeter.greet").public(),
      pair(),
      pair(),
      &["message", "count"],
      |(name, n), _env| succeed((format!("Hello, {name}!"), n)),
    );
    let ok = call(
      &group,
      "Greeter.greet",
      serde_json::json!({"name": "Ada", "n": 2}),
    )
    .await;
    assert_eq!(
      ok.exit,
      WireExit::Success {
        value: serde_json::json!({"message": "Hello, Ada!", "count": 2})
      }
    );
    let bad = call(&group, "Greeter.greet", serde_json::json!({"name": 1})).await;
    let Some(RpcFailure::InvalidInput { issues }) = bad.reserved() else {
      panic!("expected InvalidInput");
    };
    assert_eq!(issues[0].path, "/name");
    assert_eq!(issues.len(), 2, "every issue at once");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn typed_failures_and_defects_map_to_exits() {
    let mut group = RpcGroup::new();
    group.register(
      Procedure::query("A.missing").public(),
      pair(),
      pair(),
      &["name", "n"],
      |_, _| id_effect::fail(RpcError::not_found("nope")),
    );
    group.register(
      Procedure::mutation("A.boom").public(),
      pair(),
      pair(),
      &["name", "n"],
      |_: (String, i64), _| die("handler bug"),
    );
    group.register(
      Procedure::mutation("A.badout").public(),
      pair(),
      pair(),
      &["only"],
      |v, _| succeed(v),
    );
    let ok_payload = serde_json::json!({"name": "Ada", "n": 1});
    let missing = call(&group, "A.missing", ok_payload.clone()).await;
    assert_eq!(missing.status, http::StatusCode::NOT_FOUND);
    assert_eq!(
      missing.exit,
      WireExit::Failure {
        error: serde_json::json!({"_tag": "NotFound", "message": "nope"})
      }
    );
    let boom = call(&group, "A.boom", ok_payload.clone()).await;
    assert_eq!(
      boom.reserved(),
      Some(RpcFailure::InternalError { id: "cid-1".into() })
    );
    assert!(
      !serde_json::to_string(&boom.exit)
        .unwrap()
        .contains("handler bug")
    );
    let badout = call(&group, "A.badout", ok_payload).await;
    assert_eq!(badout.status, http::StatusCode::INTERNAL_SERVER_ERROR);
  }

  #[test]
  fn procedures_record_kind_version_access_and_guards() {
    let undecided = Procedure::query("A.q");
    assert_eq!(undecided.route_access(), RouteAccess::Undecided);
    assert_eq!(undecided.kind(), ProcedureKind::Query);
    assert_eq!(undecided.tag(), "A.q");
    let public = Procedure::mutation("A.m").describe("m").public();
    assert_eq!(public.route_access(), RouteAccess::Public);
    let guarded = public.guard(Staff).guard(Staff);
    assert_eq!(guarded.route_access(), RouteAccess::Guarded);
    let debug = format!("{guarded:?}");
    assert!(debug.contains("A.m") && debug.contains("staff"));
    for (p, kind) in [
      (Procedure::server_stream("S.s"), ProcedureKind::ServerStream),
      (Procedure::client_stream("S.c"), ProcedureKind::ClientStream),
      (Procedure::bidi("S.b"), ProcedureKind::Bidi),
      (Procedure::subscription("S.u"), ProcedureKind::Subscription),
    ] {
      assert_eq!(p.kind(), kind);
    }

    let mut group = RpcGroup::new();
    assert!(group.is_empty());
    group.register(
      Procedure::query("A.q").describe("read").public(),
      pair(),
      pair(),
      &["name", "n"],
      |v, _| succeed(v),
    );
    group.register(guarded, pair(), pair(), &["name", "n"], |v, _| succeed(v));
    group.register(
      Procedure::query("Other.q").public(),
      pair(),
      pair(),
      &["name", "n"],
      |v, _| succeed(v),
    );
    group.query(
      Procedure::query("A.q").version(2).public(),
      WireSchema::new(schema::string::<()>()),
      WireSchema::new(schema::string::<()>()),
      |s: String, _: &mut DynEnv| succeed::<_, Never, _>(s),
    );
    group.client_stream(
      Procedure::client_stream("A.c")
        .max_items(3)
        .max_bytes(9)
        .public(),
      WireSchema::new(schema::i64::<()>()),
      WireSchema::new(schema::i64::<()>()),
      |s: id_effect::Stream<i64, Never, DynEnv>, _: &mut DynEnv| s.run_fold(0, |a, b| a + b),
    );
    assert_eq!(group.len(), 5);
    assert_eq!(
      group.tags().collect::<Vec<_>>(),
      ["A.c", "A.m", "A.q", "Other.q"]
    );
    assert_eq!(group.get("A.q").unwrap().version(), 2);
    assert_eq!(group.get_version("A.q", 1).unwrap().version(), 1);
    assert!(group.get_version("A.q", 3).is_none());
    let entry = group.get("A.m").unwrap();
    assert_eq!(entry.tag(), "A.m");
    assert_eq!(entry.kind(), ProcedureKind::Mutation);
    assert_eq!(entry.guards().len(), 2);
    assert_eq!(
      entry.access(),
      &ProcedureAccess::Guarded {
        policies: vec!["staff".into(), "staff".into()]
      }
    );
    assert!(format!("{entry:?}").contains("Mutation"));
    assert!(format!("{group:?}").contains("Other.q"));
    let stream = group.get("A.c").unwrap();
    assert_eq!((stream.max_items(), stream.max_bytes()), (3, 9));

    let service = group.service_contract("A", 1);
    let names: Vec<_> = service.procedures.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["c", "m", "q"]);
    let q = service.procedure("q").unwrap();
    assert!(q.access.is_public());
    assert_eq!(q.description.as_deref(), Some("read"));
    assert_eq!(q.input["type"], "object");
    assert!(q.error.as_ref().unwrap()["oneOf"].is_array());
    assert!(service.procedure("m").unwrap().idempotent);
    assert_eq!(service.procedure("c").unwrap().max_items, Some(3));
    let v2 = group.service_contract("A", 2);
    assert_eq!(v2.procedures.len(), 1);
    assert!(v2.procedures[0].error.is_none());
  }

  #[test]
  fn tags_are_validated() {
    assert!(is_valid_tag("Greeter.greet-v2_x"));
    assert!(!is_valid_tag(""));
    assert!(!is_valid_tag("a b"));
    assert!(!is_valid_tag(&"a".repeat(MAX_TAG_LEN + 1)));
    assert!(std::panic::catch_unwind(|| Procedure::query("bad tag")).is_err());
    assert!(std::panic::catch_unwind(|| Procedure::query("A.q").version(0)).is_err());
  }

  #[test]
  #[should_panic(expected = "registered twice")]
  fn duplicate_tags_are_refused() {
    let mut group = RpcGroup::new();
    for _ in 0..2 {
      group.register(
        Procedure::query("A.q").public(),
        pair(),
        pair(),
        &["name", "n"],
        |v, _| succeed(v),
      );
    }
  }

  #[test]
  #[should_panic(expected = "registers query")]
  fn kinds_must_match_the_registration_method() {
    let mut group = RpcGroup::new();
    group.query(
      Procedure::mutation("A.m").public(),
      WireSchema::new(schema::string::<()>()),
      WireSchema::new(schema::string::<()>()),
      |s: String, _: &mut DynEnv| succeed::<_, Never, _>(s),
    );
  }
}
