//! Extra HTTP integration tests (verbs, validation, body cap) for [`ReqwestHttpClient`].

mod common;

use common::run;
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClientEnv, HttpMethod, HttpRequest, ReqwestHttpClient, execute, http_client_env,
  provide_reqwest_http_client,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn env() -> HttpClientEnv {
  http_client_env(ReqwestHttpClient::default_client())
}

fn request(method: HttpMethod, url: String, body: Option<&[u8]>) -> HttpRequest {
  HttpRequest {
    method,
    url,
    headers: vec![],
    body: body.map(<[u8]>::to_vec),
    timeout: None,
    max_body_bytes: None,
  }
}

#[tokio::test]
async fn execute_put_patch_delete_round_trip() {
  let server = MockServer::start().await;
  for (verb, route) in [("PUT", "/p"), ("PATCH", "/a"), ("DELETE", "/d")] {
    Mock::given(method(verb))
      .and(path(route))
      .respond_with(ResponseTemplate::new(204))
      .mount(&server)
      .await;
  }
  let base = server.uri();
  for req in [
    request(HttpMethod::Put, format!("{base}/p"), Some(b"u")),
    request(HttpMethod::Patch, format!("{base}/a"), Some(b"x")),
    request(HttpMethod::Delete, format!("{base}/d"), None),
  ] {
    let r = run(execute(req).provide_all(env())).await.expect("verb");
    assert_eq!(r.status, 204);
  }
}

#[tokio::test]
async fn execute_rejects_invalid_header_name() {
  let server = MockServer::start().await;
  let mut req = request(HttpMethod::Get, format!("{}/n", server.uri()), None);
  req.headers = vec![("not a token name\n".to_string(), "v".to_string())];
  let err = run(execute(req).provide_all(env()))
    .await
    .expect_err("invalid header");
  assert!(matches!(err, HttpError::InvalidRequest(_)));
}

#[tokio::test]
async fn execute_rejects_body_larger_than_max() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/big"))
    .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 64]))
    .mount(&server)
    .await;
  let mut req = request(HttpMethod::Get, format!("{}/big", server.uri()), None);
  req.max_body_bytes = Some(8);
  let err = run(execute(req).provide_all(env()))
    .await
    .expect_err("body cap");
  assert!(matches!(err, HttpError::BodyTooLarge { .. }));
}

#[tokio::test]
async fn reqwest_http_client_layer_builds_env() {
  let built = run(provide_reqwest_http_client::<HttpError>().build()).await;
  assert!(built.is_ok());
}

#[tokio::test]
async fn execute_rejects_invalid_header_value() {
  let server = MockServer::start().await;
  let mut req = request(HttpMethod::Get, format!("{}/v", server.uri()), None);
  req.headers = vec![("X-Ok".to_string(), "bad\nvalue".to_string())];
  let err = run(execute(req).provide_all(env()))
    .await
    .expect_err("header value");
  assert!(matches!(err, HttpError::InvalidRequest(_)));
}

#[tokio::test]
async fn execute_connection_error_surfaces_as_reqwest() {
  let req = HttpRequest::get("http://127.0.0.1:1/connection-refused");
  let err = run(execute(req).provide_all(env()))
    .await
    .expect_err("refused");
  assert!(matches!(err, HttpError::Reqwest(_)));
}
