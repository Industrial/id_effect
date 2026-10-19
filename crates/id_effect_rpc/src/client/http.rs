//! `POST /rpc` calls through id_effect_platform's [`HttpClientService`], in the 0.12 wire
//! format: a [`WireCall`] envelope out, a [`WireResult`] (or a Problem) back.

use id_effect::schema::Schema;
use id_effect::schema::data::EffectData;
use id_effect::{Effect, HList, Selector, fail, need};
use id_effect_axum::route::Problem;
use id_effect_platform::http::{HttpClientService, HttpMethod, HttpRequest};

use super::{RPC_HEADER, RpcClient, RpcClientError};
use crate::serialization::{
  RpcSerializationError, StructWireJson, decode_payload, encode_struct_fields,
};
use crate::wire::{WireCall, WireExit, WireResult};

impl RpcClient {
  /// Call the procedure `tag` with a schema-encoded `payload` (`payload_fields` names the
  /// members of the encoded object) and decode its output with `success_schema`.
  ///
  /// A typed or reserved failure is [`RpcClientError::Failure`] (the `{"_tag": …}` value); a
  /// refusal before the procedure ran is [`RpcClientError::Refused`] with the server's Problem.
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
    let wire = WireCall::new(tag, payload);
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

/// Decode a response: a [`WireResult`] (success against `schema`, failure as
/// [`RpcClientError::Failure`]), a Problem as [`RpcClientError::Refused`], anything else by
/// status.
pub fn decode_response<B, IB, EB>(
  schema: &Schema<B, IB, EB>,
  status: u16,
  body: &[u8],
) -> Result<B, RpcClientError>
where
  B: 'static,
  IB: 'static,
  EB: EffectData + 'static,
{
  if let Ok(result) = serde_json::from_slice::<WireResult>(body) {
    return match result.exit {
      WireExit::Success { value } => {
        decode_payload(schema, &value).map_err(RpcClientError::Serialization)
      }
      WireExit::Failure { error } => Err(RpcClientError::Failure(error)),
    };
  }
  if let Ok(problem) = serde_json::from_slice::<Problem>(body) {
    return Err(RpcClientError::Refused(Box::new(problem)));
  }
  if status != 200 {
    return Err(RpcClientError::UnexpectedStatus(status));
  }
  Err(RpcClientError::Serialization(
    RpcSerializationError::Protocol(format!(
      "a 200 response that is not an rpc result ({} bytes)",
      body.len()
    )),
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
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
  fn call_posts_a_wire_call_to_the_dispatch_url() {
    let body = br#"{"tag":"Greeter.greet","exit":{"_tag":"Success","value":{"message":"Hello!","count":1}}}"#;
    let (out, mock) = call_with(200, body.to_vec());
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
    let failure = br#"{"tag":"Greeter.greet","exit":{"_tag":"Failure","error":{"_tag":"NotFound","message":"nope"}}}"#;
    let (out, _) = call_with(404, failure.to_vec());
    let err = out.unwrap_err();
    assert_eq!(err.failure_tag(), Some("NotFound"));

    let (out, _) = call_with(404, br#"{"status":404,"title":"Not Found"}"#.to_vec());
    assert!(matches!(out, Err(RpcClientError::Refused(p)) if p.status.as_u16() == 404));

    let (out, _) = call_with(502, b"bad gateway".to_vec());
    assert!(matches!(out, Err(RpcClientError::UnexpectedStatus(502))));

    let (out, _) = call_with(200, b"not json".to_vec());
    assert!(matches!(out, Err(RpcClientError::Serialization(_))));

    let bad = br#"{"tag":"Greeter.greet","exit":{"_tag":"Success","value":{"message":1}}}"#;
    let (out, _) = call_with(200, bad.to_vec());
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
