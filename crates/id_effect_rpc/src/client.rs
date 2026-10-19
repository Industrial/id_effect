//! RPC HTTP client — call remote tagged methods as `Effect` (`RpcClient` parity).

use id_effect::schema::Schema;
use id_effect::schema::data::EffectData;
use id_effect::{Effect, HList, Selector, fail, need};
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

/// Typed RPC client using [`id_effect_platform::http::HttpClient`] ([`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc) `RpcClient` parity).
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
  pub fn call<A, IA, EA, B, IB, EB, R, I>(
    &self,
    tag: impl Into<String>,
    payload: A,
    payload_schema: &Schema<A, IA, EA>,
    payload_fields: &[&str],
    success_schema: &Schema<B, IB, EB>,
    success_fields: &[&str],
  ) -> Effect<B, RpcClientError, R>
  where
    R: Selector<HttpClientService, I> + HList,
    A: Send + 'static,
    B: Send + 'static,
    EA: EffectData + 'static,
    EB: EffectData + 'static,
    IA: Send + Sync + 'static + StructWireJson,
    IB: Send + Sync + 'static,
  {
    let tag = tag.into();
    let config = self.config.clone();
    let payload_json = match encode_struct_fields(payload_schema, payload_fields, payload) {
      Ok(v) => v,
      Err(e) => return fail(RpcClientError::Serialization(e)),
    };
    let wire_req = RpcWireRequest {
      tag: tag.clone(),
      payload: payload_json,
    };
    let body = match serde_json::to_vec(&wire_req) {
      Ok(v) => v,
      Err(e) => return fail(RpcClientError::Serialization(e.into())),
    };
    let success_schema = success_schema.clone();
    let _success_fields = success_fields;

    let req = HttpRequest {
      method: HttpMethod::Post,
      url: config.url(),
      headers: vec![("content-type".to_owned(), "application/json".to_owned())],
      body: Some(body),
      timeout: None,
      max_body_bytes: None,
    };
    need::<HttpClientService>().flat_map(move |client| {
      client
        .execute(req)
        .map_err(RpcClientError::Http)
        .flat_map(move |resp| {
          let decoded = if resp.status != 200 {
            Err(RpcClientError::UnexpectedStatus(resp.status))
          } else {
            decode_success(&success_schema, &resp.body)
          };
          match decoded {
            Ok(b) => id_effect::succeed(b),
            Err(e) => fail(e),
          }
        })
        .narrow()
    })
  }
}

/// Decodes a 200 response body: the success payload against `schema`, or the failure envelope.
fn decode_success<B, IB, EB>(schema: &Schema<B, IB, EB>, body: &[u8]) -> Result<B, RpcClientError>
where
  B: 'static,
  IB: 'static,
  EB: EffectData + 'static,
{
  let wire: RpcWireResponse =
    serde_json::from_slice(body).map_err(|e| RpcClientError::Serialization(e.into()))?;
  match wire {
    RpcWireResponse::Success { success, .. } => {
      validate_success_json(schema, &success).map_err(RpcClientError::Serialization)
    }
    RpcWireResponse::Failure { failure, .. } => Err(RpcClientError::rpc_envelope(&failure)),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::hlist;
  use id_effect::schema;
  use id_effect_platform::http::{HttpClient, HttpResponse};
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
    ) -> id_effect::Effect<HttpResponse, id_effect_platform::error::HttpError> {
      let url = req.url.clone();
      *self.last_url.lock().unwrap() = Some(url);
      let resp = self.response.lock().unwrap().clone().expect("response set");
      id_effect::succeed(resp)
    }

    fn execute_stream(
      &self,
      _req: HttpRequest,
    ) -> id_effect::Effect<
      id_effect_platform::http::StreamingHttpResponse,
      id_effect_platform::error::HttpError,
    > {
      id_effect::fail(id_effect_platform::error::HttpError::InvalidRequest(
        "mock stream".to_owned(),
      ))
    }
  }

  #[test]
  fn call_posts_tagged_json_to_dispatch_url() {
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
    let env = hlist![HttpClientService(mock.clone() as Arc<dyn HttpClient>)];
    let client = RpcClient::new("http://127.0.0.1:8080");
    let out = client
      .call(
        "greet",
        ("Ada".to_owned(), 1_i64),
        &payload,
        &["name", "enthusiasm"],
        &success,
        &["message", "count"],
      )
      .provide_all(env)
      .block_on()
      .into_result()
      .expect("call");
    assert_eq!(out.0, "Hello!");
    assert_eq!(
      mock.last_url.lock().unwrap().as_deref(),
      Some("http://127.0.0.1:8080/rpc")
    );
  }
}
