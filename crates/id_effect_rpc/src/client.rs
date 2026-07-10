//! RPC HTTP client — call remote tagged methods as `Effect` (`RpcClient` parity).

use id_effect::Effect;
use id_effect::Needs;
use id_effect::data::EffectData;
use id_effect::schema::Schema;
use id_effect_platform::http::{HttpClientService, HttpMethod, HttpRequest};

use crate::protocol::{RPC_DISPATCH_PATH, RpcWireRequest, RpcWireResponse};
use crate::serialization::{
  RpcSerializationError, StructWireJson, encode_struct_fields, validate_success_json,
};

/// Client-side RPC failures (wire, HTTP, or domain [`crate::RpcError`] mapped from failure envelope).
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
  /// Wire encode/decode or schema validation failed.
  #[error("serialization: {0}")]
  Serialization(#[from] RpcSerializationError),
  /// Platform HTTP transport error.
  #[error("http: {0}")]
  Http(#[from] id_effect_platform::error::HttpError),
  /// Remote returned a structured RPC failure envelope.
  #[error("rpc: {0}")]
  Rpc(String),
  /// HTTP status was not 200 OK.
  #[error("unexpected status {0}")]
  UnexpectedStatus(u16),
}

impl RpcClientError {
  fn rpc_envelope(env: &crate::envelope::RpcEnvelope) -> Self {
    Self::Rpc(env.message.clone())
  }
}

/// Configuration for [`RpcClient`].
#[derive(Clone, Debug)]
pub struct RpcClientConfig {
  /// Base URL (e.g. `http://127.0.0.1:8080`).
  pub base_url: String,
  /// Dispatch path (default [`RPC_DISPATCH_PATH`]).
  pub dispatch_path: String,
}

impl RpcClientConfig {
  /// New config with default `/rpc` path.
  #[inline]
  pub fn new(base_url: impl Into<String>) -> Self {
    Self {
      base_url: base_url.into(),
      dispatch_path: RPC_DISPATCH_PATH.to_owned(),
    }
  }

  fn url(&self) -> String {
    let base = self.base_url.trim_end_matches('/');
    format!("{base}{}", self.dispatch_path)
  }
}

/// Typed RPC client using [`HttpClient`] ([`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc) `RpcClient` parity).
#[derive(Clone, Debug)]
pub struct RpcClient {
  config: RpcClientConfig,
}

impl RpcClient {
  /// Create a client for `base_url`.
  #[inline]
  pub fn new(base_url: impl Into<String>) -> Self {
    Self {
      config: RpcClientConfig::new(base_url),
    }
  }

  /// Override dispatch path (default `/rpc`).
  #[must_use]
  pub fn with_dispatch_path(mut self, path: impl Into<String>) -> Self {
    self.config.dispatch_path = path.into();
    self
  }

  /// Call tagged RPC method with schema-validated payload and success type.
  pub fn call<A, IA, EA, B, IB, EB, R>(
    &self,
    tag: impl Into<String>,
    payload: A,
    payload_schema: &Schema<A, IA, EA>,
    payload_fields: &[&str],
    success_schema: &Schema<B, IB, EB>,
    success_fields: &[&str],
  ) -> Effect<B, RpcClientError, R>
  where
    A: Send + 'static,
    B: Send + 'static,
    EA: EffectData + 'static,
    EB: EffectData + 'static,
    IA: Send + Sync + 'static + StructWireJson,
    IB: Send + Sync + 'static,
    R: Needs<HttpClientService> + Send + 'static,
  {
    let tag = tag.into();
    let config = self.config.clone();
    let payload_json = match encode_struct_fields(payload_schema, payload_fields, payload) {
      Ok(v) => v,
      Err(e) => return id_effect::fail(RpcClientError::Serialization(e)),
    };
    let wire_req = RpcWireRequest {
      tag: tag.clone(),
      payload: payload_json,
    };
    let body = match serde_json::to_vec(&wire_req) {
      Ok(v) => v,
      Err(e) => return id_effect::fail(RpcClientError::Serialization(e.into())),
    };
    let success_schema = success_schema.clone();
    let _success_fields = success_fields;

    Effect::new_async(move |r: &mut R| {
      let config = config.clone();
      let body = body.clone();
      let success_schema = success_schema.clone();
      Box::pin(async move {
        let req = HttpRequest {
          method: HttpMethod::Post,
          url: config.url(),
          headers: vec![("content-type".to_owned(), "application/json".to_owned())],
          body: Some(body),
          timeout: None,
          max_body_bytes: None,
        };
        let client = r.need().clone();
        let resp = client.execute(req).run(&mut ()).await?;
        if resp.status != 200 {
          return Err(RpcClientError::UnexpectedStatus(resp.status));
        }
        let wire: RpcWireResponse = serde_json::from_slice(&resp.body)
          .map_err(|e| RpcClientError::Serialization(e.into()))?;
        match wire {
          RpcWireResponse::Success { success, .. } => {
            validate_success_json(&success_schema, &success).map_err(RpcClientError::Serialization)
          }
          RpcWireResponse::Failure { failure, .. } => Err(RpcClientError::rpc_envelope(&failure)),
        }
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Env;
  use id_effect::schema;
  use id_effect_platform::http::{HttpClient, HttpResponse, env_set_http_client};
  use std::sync::Arc;
  use std::sync::Mutex;

  struct MockHttp {
    response: Mutex<Option<HttpResponse>>,
    last_url: Mutex<Option<String>>,
  }

  impl HttpClient for MockHttp {
    fn execute(
      &self,
      req: HttpRequest,
    ) -> Effect<HttpResponse, id_effect_platform::error::HttpError, ()> {
      let url = req.url.clone();
      *self.last_url.lock().unwrap() = Some(url);
      let resp = self.response.lock().unwrap().clone().expect("response set");
      Effect::new(move |_r| Ok(resp))
    }

    fn execute_stream(
      &self,
      _req: HttpRequest,
    ) -> Effect<
      id_effect_platform::http::StreamingHttpResponse,
      id_effect_platform::error::HttpError,
      (),
    > {
      Effect::new(|_r| {
        Err(id_effect_platform::error::HttpError::InvalidRequest(
          "mock stream".to_owned(),
        ))
      })
    }
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn call_posts_tagged_json_to_dispatch_url() {
    let payload = schema::struct_(
      "name",
      schema::string::<()>(),
      "enthusiasm",
      schema::i64::<()>(),
    );
    let success = schema::struct_(
      "message",
      schema::string::<()>(),
      "count",
      schema::i64::<()>(),
    );
    let wire = RpcWireResponse::success(
      "greet",
      serde_json::json!({"message": "Hello!", "count": 1}),
    );
    let mock = Arc::new(MockHttp {
      response: Mutex::new(Some(HttpResponse {
        status: 200,
        headers: vec![],
        body: serde_json::to_vec(&wire).unwrap(),
      })),
      last_url: Mutex::new(None),
    });
    let mut env = Env::new();
    env_set_http_client(&mut env, mock.clone() as Arc<dyn HttpClient>);
    let client = RpcClient::new("http://127.0.0.1:8080");
    let out = id_effect::run_async(
      client.call(
        "greet",
        ("Ada".to_owned(), 1_i64),
        &payload,
        &["name", "enthusiasm"],
        &success,
        &["message", "count"],
      ),
      env,
    )
    .await
    .expect("call");
    assert_eq!(out.0, "Hello!");
    assert_eq!(
      mock.last_url.lock().unwrap().as_deref(),
      Some("http://127.0.0.1:8080/rpc")
    );
  }
}
