//! The procedure registry: an [`RpcGroup`] of tagged procedures, each with an access decision
//! (ADR 0014 C1, the H8 rule of typed routes applied per procedure).
//!
//! A [`Procedure`] starts [`Undecided`]; [`Procedure::public`] or [`Procedure::guard`] decides
//! who may call it, and [`RpcGroup::register`] accepts only a decided procedure, so forgetting
//! the decision is a compile error rather than an open procedure:
//!
//! ```rust
//! use std::sync::Arc;
//! use id_effect::{schema, succeed};
//! use id_effect_rpc::{Procedure, RpcGroup};
//!
//! let mut group = RpcGroup::new();
//! group.register(
//!   Procedure::query("Greeter.greet").public(),
//!   Arc::new(schema::struct_("name", schema::string::<()>(), "times", schema::i64::<()>())),
//!   Arc::new(schema::struct_("message", schema::string::<()>(), "times", schema::i64::<()>())),
//!   &["message", "times"],
//!   |(name, times), _env| succeed((format!("Hello, {name}!"), times)),
//! );
//! assert_eq!(group.tags().collect::<Vec<_>>(), ["Greeter.greet"]);
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
//! the dispatcher runs them on the request head, outermost last, before the payload is decoded.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use id_effect::schema::Schema;
use id_effect::schema::data::EffectData;
use id_effect::{DynEnv, Effect, Exit};
use id_effect_axum::route::RouteAccess;
pub use id_effect_axum::route::{Access, Decided, Guarded, Public, RouteGuard, Undecided};
use serde_json::Value;

use crate::RpcError;
use crate::contract::{ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract};
use crate::protocol::RpcWireResponse;
use crate::serialization::{
  RpcSerializationError, StructWireJson, decode_payload, rpc_error_to_wire, success_wire,
};

/// The longest procedure tag.
pub const MAX_TAG_LEN: usize = 128;

type BoxDispatch = Arc<
  dyn Fn(
      Value,
      DynEnv,
    ) -> Pin<Box<dyn Future<Output = Result<RpcWireResponse, RpcSerializationError>> + Send>>
    + Send
    + Sync,
>;

/// Whether `tag` is a valid procedure tag: 1 to [`MAX_TAG_LEN`] characters of
/// `[A-Za-z0-9_.-]` (`"<Service>.<procedure>"` by convention).
pub fn is_valid_tag(tag: &str) -> bool {
  !tag.is_empty()
    && tag.len() <= MAX_TAG_LEN
    && tag
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// One procedure before registration: its tag, kind and access decision.
///
/// `A` is the access state: [`Undecided`] until [`Procedure::public`] or [`Procedure::guard`],
/// then [`Public`] or [`Guarded`]; [`RpcGroup::register`] only accepts a decided procedure.
#[must_use = "a procedure does nothing until it is registered in an `RpcGroup`"]
pub struct Procedure<A = Undecided> {
  tag: String,
  kind: ProcedureKind,
  description: Option<String>,
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
      description: None,
      guards: Vec::new(),
      access: PhantomData,
    }
  }

  /// A retry-safe, read-only procedure.
  ///
  /// # Panics
  ///
  /// When `tag` is not a valid tag ([`is_valid_tag`]).
  pub fn query(tag: impl Into<String>) -> Self {
    Self::new(tag, ProcedureKind::Query)
  }

  /// A state-changing procedure.
  ///
  /// # Panics
  ///
  /// When `tag` is not a valid tag ([`is_valid_tag`]).
  pub fn mutation(tag: impl Into<String>) -> Self {
    Self::new(tag, ProcedureKind::Mutation)
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
      description: self.description,
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
      .field(
        "policies",
        &self.guards.iter().map(|g| g.policy()).collect::<Vec<_>>(),
      )
      .finish()
  }
}

/// One registered procedure: tag, kind, access decision, guards, schemas and dispatch closure.
#[derive(Clone)]
pub struct RpcMethodEntry {
  /// Stable procedure tag.
  pub tag: String,
  kind: ProcedureKind,
  access: ProcedureAccess,
  guards: Vec<Arc<dyn RouteGuard>>,
  input: Value,
  output: Value,
  description: Option<String>,
  inner: BoxDispatch,
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

  /// The access decision recorded in the contract.
  pub fn access(&self) -> &ProcedureAccess {
    &self.access
  }

  /// The guards, outermost last (empty for a public procedure).
  pub fn guards(&self) -> &[Arc<dyn RouteGuard>] {
    &self.guards
  }

  /// The procedure's contract entry (`name` = `name`; the low-level group declares no typed
  /// error union, so `error` is `null`).
  pub fn contract(&self, name: impl Into<String>) -> ProcedureContract {
    let mut procedure = ProcedureContract::new(
      name,
      self.kind,
      self.access.clone(),
      self.input.clone(),
      self.output.clone(),
    );
    procedure.description = self.description.clone();
    procedure
  }

  /// Dispatch a JSON payload through the registered handler (guards are the caller's job: see
  /// [`crate::server::dispatch`]).
  pub async fn dispatch(
    &self,
    payload: Value,
    env: DynEnv,
  ) -> Result<RpcWireResponse, RpcSerializationError> {
    (self.inner)(payload, env).await
  }
}

impl fmt::Debug for RpcMethodEntry {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RpcMethodEntry")
      .field("tag", &self.tag)
      .field("kind", &self.kind)
      .field("access", &self.access)
      .finish_non_exhaustive()
  }
}

/// Registered procedures keyed by tag ([`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc)
/// `RpcGroup` parity), every one with an access decision.
#[derive(Clone, Default)]
pub struct RpcGroup {
  methods: BTreeMap<String, RpcMethodEntry>,
}

impl RpcGroup {
  /// Empty group.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a unary procedure (a [`query`](Procedure::query) or a
  /// [`mutation`](Procedure::mutation)) with its input and output schemas; `output_fields`
  /// names the members of the encoded output object.
  ///
  /// Each dispatch runs the handler's effect as one request fiber
  /// ([`id_effect_axum::run_with_env`]). A typed [`RpcError`] becomes a failure envelope; a
  /// defect (a panic in `handler` or its effect) or an interrupt becomes an
  /// [`RpcError::internal`] envelope (the defect message is not sent).
  ///
  /// # Panics
  ///
  /// When a procedure with the same tag is already registered.
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
    let Procedure {
      tag,
      kind,
      description,
      guards,
      ..
    } = procedure;
    assert!(
      !self.methods.contains_key(&tag),
      "rpc procedure {tag:?} is registered twice"
    );
    let access = match D::ACCESS {
      RouteAccess::Guarded => ProcedureAccess::Guarded {
        policies: guards.iter().map(|g| g.policy().to_owned()).collect(),
      },
      _ => ProcedureAccess::Public {},
    };
    let input_schema = input.json_schema();
    let output_schema = output.json_schema();
    let handler = Arc::new(handler);
    let dispatch_tag = tag.clone();
    let dispatch: BoxDispatch = Arc::new(move |payload, env| {
      let tag = dispatch_tag.clone();
      let input = input.clone();
      let output = output.clone();
      let handler = handler.clone();
      Box::pin(async move {
        let value = decode_payload(input.as_ref(), &payload)?;
        let exit = id_effect_axum::run_with_env(env, move |e| handler(value, e)).await;
        match exit {
          Exit::Success(value) => success_wire(&tag, output.as_ref(), output_fields, value),
          Exit::Failure(cause) => match cause.fail_option() {
            Some(err) => Ok(rpc_error_to_wire(&tag, err)),
            None => Ok(rpc_error_to_wire(
              &tag,
              RpcError::internal("rpc handler defect or interrupt"),
            )),
          },
        }
      })
    });
    self.methods.insert(
      tag.clone(),
      RpcMethodEntry {
        tag,
        kind,
        access,
        guards,
        input: input_schema,
        output: output_schema,
        description,
        inner: dispatch,
      },
    );
  }

  /// Lookup a procedure by tag.
  #[inline]
  pub fn get(&self, tag: &str) -> Option<&RpcMethodEntry> {
    self.methods.get(tag)
  }

  /// All registered tags, sorted.
  pub fn tags(&self) -> impl Iterator<Item = &str> {
    self.methods.keys().map(String::as_str)
  }

  /// Every registered procedure, sorted by tag.
  pub fn entries(&self) -> impl Iterator<Item = &RpcMethodEntry> {
    self.methods.values()
  }

  /// Number of registered procedures.
  #[inline]
  pub fn len(&self) -> usize {
    self.methods.len()
  }

  /// Whether the group has no procedures.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.methods.is_empty()
  }

  /// The contract of service `name` at `version`: every procedure whose tag starts with
  /// `"<name>."`, named by the rest of its tag.
  pub fn service_contract(&self, name: &str, version: u32) -> ServiceContract {
    let prefix = format!("{name}.");
    self
      .entries()
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
  use axum::http::request::Parts;
  use id_effect::schema;
  use id_effect::{die, succeed};
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

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn register_and_dispatch_greet() {
    let mut group = RpcGroup::new();
    group.register(
      Procedure::query("Greeter.greet").public(),
      pair(),
      pair(),
      &["message", "count"],
      |(name, n), _env| succeed((format!("Hello, {name}!"), n)),
    );
    let method = group.get("Greeter.greet").expect("method");
    let wire = method
      .dispatch(serde_json::json!({"name": "Ada", "n": 2}), DynEnv::new())
      .await
      .expect("dispatch");
    match wire {
      RpcWireResponse::Success { success, .. } => {
        assert_eq!(success["message"], "Hello, Ada!");
        assert_eq!(success["count"], 2);
      }
      RpcWireResponse::Failure { .. } => panic!("expected success"),
    }
    let bad = method
      .dispatch(serde_json::json!({"name": 1}), DynEnv::new())
      .await;
    assert!(matches!(bad, Err(RpcSerializationError::Schema { .. })));
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn typed_failure_and_defect_become_failure_envelopes() {
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
    let ok_payload = serde_json::json!({"name": "Ada", "n": 1});
    match group
      .get("A.missing")
      .unwrap()
      .dispatch(ok_payload.clone(), DynEnv::new())
      .await
      .expect("dispatch")
    {
      RpcWireResponse::Failure { failure, .. } => {
        assert_eq!(failure.code, crate::RpcErrorCode::NotFound)
      }
      RpcWireResponse::Success { .. } => panic!("expected failure"),
    }
    match group
      .get("A.boom")
      .unwrap()
      .dispatch(ok_payload, DynEnv::new())
      .await
      .expect("dispatch")
    {
      RpcWireResponse::Failure { failure, .. } => {
        assert_eq!(failure.code, crate::RpcErrorCode::Internal);
        assert!(!failure.message.contains("handler bug"));
      }
      RpcWireResponse::Success { .. } => panic!("expected failure"),
    }
  }

  #[test]
  fn procedures_record_kind_access_and_guards() {
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

    let mut group = RpcGroup::new();
    assert!(group.is_empty());
    group.register(guarded, pair(), pair(), &["name", "n"], |v, _| succeed(v));
    group.register(
      Procedure::query("A.q").public().describe("read"),
      pair(),
      pair(),
      &["name", "n"],
      |v, _| succeed(v),
    );
    group.register(
      Procedure::query("Other.q").public(),
      pair(),
      pair(),
      &["name", "n"],
      |v, _| succeed(v),
    );
    assert_eq!(group.len(), 3);
    assert_eq!(group.tags().collect::<Vec<_>>(), ["A.m", "A.q", "Other.q"]);
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

    let service = group.service_contract("A", 1);
    let names: Vec<_> = service.procedures.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["m", "q"]);
    let q = service.procedure("q").unwrap();
    assert!(q.access.is_public());
    assert_eq!(q.description.as_deref(), Some("read"));
    assert_eq!(q.input["type"], "object");
    assert!(q.error.is_none());
  }

  #[test]
  fn tags_are_validated() {
    assert!(is_valid_tag("Greeter.greet-v2_x"));
    assert!(!is_valid_tag(""));
    assert!(!is_valid_tag("a b"));
    assert!(!is_valid_tag(&"a".repeat(MAX_TAG_LEN + 1)));
    assert!(std::panic::catch_unwind(|| Procedure::query("bad tag")).is_err());
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
}
