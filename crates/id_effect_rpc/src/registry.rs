//! RPC method registry — [] of tagged handlers ( [] parity).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use id_effect::Env;
use id_effect::kernel::Effect;

use crate::RpcError;
use crate::protocol::RpcWireResponse;
use crate::serialization::{
  RpcSerializationError, decode_payload, rpc_error_to_wire, success_wire,
};
use id_effect::data::EffectData;
use id_effect::schema::Schema;
use id_effect_axum::run_with_env;
use serde_json::Value;

type BoxDispatch = Arc<
  dyn Fn(
      Value,
      Env,
    ) -> Pin<Box<dyn Future<Output = Result<RpcWireResponse, RpcSerializationError>> + Send>>
    + Send
    + Sync,
>;

/// One registered RPC operation (tag + async dispatch closure).
#[derive(Clone)]
pub struct RpcMethodEntry {
  /// Stable operation tag ( tag).
  pub tag: String,
  inner: BoxDispatch,
}

impl RpcMethodEntry {
  /// Operation tag.
  #[inline]
  pub fn tag(&self) -> &str {
    &self.tag
  }

  /// Dispatch a JSON payload through the registered handler.
  pub async fn dispatch(
    &self,
    payload: Value,
    env: Env,
  ) -> Result<RpcWireResponse, RpcSerializationError> {
    (self.inner)(payload, env).await
  }
}

/// Collection of tagged RPC methods ([](https://effect-ts.github.io/effect/docs/rpc) ).
#[derive(Clone, Default)]
/// Registered RPC methods keyed by tag.
pub struct RpcGroup {
  methods: HashMap<String, RpcMethodEntry>,
}

impl RpcGroup {
  /// Empty group.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a typed method with request/response schemas and field names for JSON encoding.
  /// Register a typed RPC handler.
  pub fn register<A, IA, EA, B, IB, EB, F>(
    &mut self,
    tag: impl Into<String>,
    payload_schema: Arc<Schema<A, IA, EA>>,
    payload_fields: &'static [&'static str],
    success_schema: Arc<Schema<B, IB, EB>>,
    success_fields: &'static [&'static str],
    handler: F,
  ) where
    A: Send + 'static,
    B: Send + 'static,
    EA: EffectData + 'static,
    EB: EffectData + 'static,
    IA: Send + Sync + 'static + crate::serialization::StructWireJson,
    IB: Send + Sync + 'static + crate::serialization::StructWireJson,
    F: Fn(A, &mut Env) -> Effect<B, RpcError, Env> + Send + Sync + 'static,
  {
    let tag = tag.into();
    let payload_schema2 = payload_schema.clone();
    let success_schema2 = success_schema.clone();
    let handler = Arc::new(handler);
    let tag2 = tag.clone();
    let dispatch: BoxDispatch = Arc::new(move |payload, env| {
      let tag = tag2.clone();
      let payload_schema = payload_schema2.clone();
      let success_schema = success_schema2.clone();
      let handler = handler.clone();
      let _payload_fields = payload_fields;
      let success_fields = success_fields;
      Box::pin(async move {
        let input = decode_payload(payload_schema.as_ref(), &payload)?;
        let result = run_with_env(env, |e| handler(input, e)).await;
        match result {
          Ok(value) => success_wire(&tag, success_schema.as_ref(), success_fields, value),
          Err(err) => Ok(rpc_error_to_wire(&tag, err)),
        }
      })
    });
    self.methods.insert(
      tag.clone(),
      RpcMethodEntry {
        tag,
        inner: dispatch,
      },
    );
  }

  /// Lookup a method by tag.
  #[inline]
  pub fn get(&self, tag: &str) -> Option<&RpcMethodEntry> {
    self.methods.get(tag)
  }

  /// All registered tags.
  pub fn tags(&self) -> impl Iterator<Item = &str> {
    self.methods.values().map(|m| m.tag.as_str())
  }

  /// Number of registered methods.
  #[inline]
  pub fn len(&self) -> usize {
    self.methods.len()
  }

  /// Whether the group has no methods.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.methods.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{schema, succeed};

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn register_and_dispatch_greet() {
    let payload = Arc::new(schema::struct_(
      "name",
      schema::string::<()>(),
      "enthusiasm",
      schema::i64::<()>(),
    ));
    let success = Arc::new(schema::struct_(
      "message",
      schema::string::<()>(),
      "count",
      schema::i64::<()>(),
    ));
    let mut group = RpcGroup::new();
    group.register(
      "greet",
      payload,
      &["name", "enthusiasm"],
      success,
      &["message", "count"],
      |(name, enthusiasm), _env| succeed((format!("Hello, {name}!"), enthusiasm)),
    );
    let method = group.get("greet").expect("method");
    let wire = method
      .dispatch(
        serde_json::json!({"name": "Ada", "enthusiasm": 2}),
        Env::new(),
      )
      .await
      .expect("dispatch");
    match wire {
      RpcWireResponse::Success { success, .. } => {
        assert_eq!(success["message"], "Hello, Ada!");
        assert_eq!(success["count"], 2);
      }
      RpcWireResponse::Failure { .. } => panic!("expected success"),
    }
  }
}
