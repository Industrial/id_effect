//! `POST /rpc` calls through id_effect_platform's [`HttpClientService`].

use id_effect::schema::Schema;
use id_effect::schema::data::EffectData;
use id_effect::{Effect, HList, Selector, fail, need};
use id_effect_platform::http::{HttpClientService, HttpMethod, HttpRequest};

use super::{RPC_HEADER, RpcClient, RpcClientError};
use crate::envelope::RpcEnvelope;
use crate::protocol::{RpcWireRequest, RpcWireResponse};
use crate::serialization::{StructWireJson, encode_struct_fields, validate_success_json};

impl RpcClient {
  /// Call the procedure `tag` with a schema-encoded `payload` (`payload_fields` names the
  /// members of the encoded object) and decode its output with `success_schema`.
  pub fn call<A, IA, EA, B, IB, EB, R, I>(
    &self,
    tag: impl Into<String>,
    payload: A,
    payload_schema: &Schema<A, IA, EA>,
    payload_fields: &[&str],
    success_schema: &Schema<B, IB, EB>,
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
    let payload = match encode_struct_fields(payload_schema, payload_fields, payload) {
      Ok(v) => v,
      Err(e) => return fail(RpcClientError::Serialization(e)),
    };
    let wire = RpcWireRequest {
      tag: tag.into(),
      payload,
    };
    // A tag and a `serde_json::Value` always serialize.
    let body = serde_json::to_vec(&wire).unwrap_or_default();
    let success_schema = success_schema.clone();
    let req = HttpRequest {
      method: HttpMethod::Post,
      url: self.config().url(),
      headers: vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        (RPC_HEADER.to_owned(), "1".to_owned()),
      ],
      body: Some(body),
      timeout: None,
      max_body_bytes: None,
    };
    need::<HttpClientService>().flat_map(move |client| {
      client
        .execute(req)
        .map_err(RpcClientError::Http)
        .flat_map(
          move |resp| match decode_response(&success_schema, resp.status, &resp.body) {
            Ok(b) => id_effect::succeed(b),
            Err(e) => fail(e),
          },
        )
        .narrow()
    })
  }
}

/// Decode a response: the success payload against `schema`, a failure envelope (wrapped in the
/// wire response or bare) as [`RpcClientError::Rpc`], anything else by status.
fn decode_response<B, IB, EB>(
  schema: &Schema<B, IB, EB>,
  status: u16,
  body: &[u8],
) -> Result<B, RpcClientError>
where
  B: 'static,
  IB: 'static,
  EB: EffectData + 'static,
{
  if let Ok(wire) = serde_json::from_slice::<RpcWireResponse>(body) {
    return match wire {
      RpcWireResponse::Success { success, .. } => {
        validate_success_json(schema, &success).map_err(RpcClientError::Serialization)
      }
      RpcWireResponse::Failure { failure, .. } => Err(RpcClientError::Rpc(failure)),
    };
  }
  if let Ok(envelope) = serde_json::from_slice::<RpcEnvelope>(body) {
    return Err(RpcClientError::Rpc(envelope));
  }
  if status != 200 {
    return Err(RpcClientError::UnexpectedStatus(status));
  }
  Err(RpcClientError::Serialization(
    crate::serialization::RpcSerializationError::Protocol(format!(
      "a 200 response that is not an rpc response ({} bytes)",
      body.len()
    )),
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::envelope::RpcErrorCode;
  use id_effect::hlist;
  use id_effect::schema;
  use id_effect_platform::http::{HttpClient, HttpResponse};
  use std::sync::{Arc, Mutex};

  struct MockHttp {
    response: HttpResponse,
    last: Mutex<Option<HttpRequest>>,
  }

  impl HttpClient for MockHttp {
    fn execute(
      &self,
      req: HttpRequest,
    ) -> Effect<HttpResponse, id_effect_platform::error::HttpError> {
      *self.last.lock().unwrap() = Some(req);
      id_effect::succeed(self.response.clone())
    }

    fn execute_stream(
      &self,
      _req: HttpRequest,
    ) -> Effect<id_effect_platform::http::StreamingHttpResponse, id_effect_platform::error::HttpError>
    {
      fail(id_effect_platform::error::HttpError::InvalidRequest(
        "mock stream".to_owned(),
      ))
    }
  }

  fn call_with(
    status: u16,
    body: Vec<u8>,
  ) -> (Result<(String, i64), RpcClientError>, Arc<MockHttp>) {
    let payload = schema::struct_("name", schema::string::<()>(), "n", schema::i64::<()>());
    let success = schema::struct_(
      "message",
      schema::string::<()>(),
      "count",
      schema::i64::<()>(),
    );
    let mock = Arc::new(MockHttp {
      response: HttpResponse {
        status,
        headers: vec![],
        body,
      },
      last: Mutex::new(None),
    });
    let env = hlist![HttpClientService(mock.clone() as Arc<dyn HttpClient>)];
    let out = RpcClient::new("http://127.0.0.1:8080")
      .call(
        "Greeter.greet",
        ("Ada".to_owned(), 1_i64),
        &payload,
        &["name", "n"],
        &success,
      )
      .provide_all(env)
      .block_on()
      .into_result();
    (typed(out), mock)
  }

  fn typed<A>(out: Result<A, id_effect::Cause<RpcClientError>>) -> Result<A, RpcClientError> {
    out.map_err(|cause| cause.fail_option().expect("a typed failure"))
  }

  #[test]
  fn call_posts_tagged_json_to_dispatch_url() {
    let wire = RpcWireResponse::success(
      "Greeter.greet",
      serde_json::json!({"message": "Hello!", "count": 1}),
    );
    let (out, mock) = call_with(200, serde_json::to_vec(&wire).unwrap());
    assert_eq!(out.expect("call").0, "Hello!");
    let req = mock.last.lock().unwrap().clone().unwrap();
    assert_eq!(req.url, "http://127.0.0.1:8080/rpc");
    assert!(req.headers.contains(&("x-rpc".to_owned(), "1".to_owned())));
    let sent: serde_json::Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
    assert_eq!(
      sent,
      serde_json::json!({"tag": "Greeter.greet", "payload": {"name": "Ada", "n": 1}})
    );
  }

  #[test]
  fn failures_decode_by_shape_then_status() {
    let envelope = RpcEnvelope {
      code: RpcErrorCode::NotFound,
      message: "nope".into(),
      correlation_id: None,
      details: None,
    };
    let wire = RpcWireResponse::failure("Greeter.greet", envelope.clone());
    let (out, _) = call_with(404, serde_json::to_vec(&wire).unwrap());
    assert!(matches!(out, Err(RpcClientError::Rpc(e)) if e == envelope));

    let (out, _) = call_with(400, serde_json::to_vec(&envelope).unwrap());
    assert!(matches!(out, Err(RpcClientError::Rpc(e)) if e.code == RpcErrorCode::NotFound));

    let (out, _) = call_with(404, br#"{"status":404,"title":"Not Found"}"#.to_vec());
    assert!(matches!(out, Err(RpcClientError::UnexpectedStatus(404))));

    let (out, _) = call_with(200, b"not json".to_vec());
    assert!(matches!(out, Err(RpcClientError::Serialization(_))));

    let bad_success = RpcWireResponse::success("Greeter.greet", serde_json::json!({"message": 1}));
    let (out, _) = call_with(200, serde_json::to_vec(&bad_success).unwrap());
    assert!(matches!(out, Err(RpcClientError::Serialization(_))));
  }

  #[test]
  fn unencodable_payload_fails_before_sending() {
    let payload = schema::struct_("name", schema::string::<()>(), "n", schema::i64::<()>());
    let success = schema::string::<()>();
    let mock = Arc::new(MockHttp {
      response: HttpResponse {
        status: 200,
        headers: vec![],
        body: vec![],
      },
      last: Mutex::new(None),
    });
    let env = hlist![HttpClientService(mock.clone() as Arc<dyn HttpClient>)];
    let out = RpcClient::new("http://h")
      .call(
        "A.b",
        ("x".to_owned(), 1_i64),
        &payload,
        &["only_one"],
        &success,
      )
      .provide_all(env)
      .block_on()
      .into_result();
    assert!(matches!(typed(out), Err(RpcClientError::Serialization(_))));
    assert!(mock.last.lock().unwrap().is_none());
  }
}
