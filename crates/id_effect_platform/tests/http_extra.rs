//! Extra HTTP integration tests (verbs, validation, body cap) for [`ReqwestHttpClient`].

use std::sync::Arc;

use id_effect::{Env, build_env, run_async};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClient, HttpMethod, HttpRequest, ReqwestHttpClient, env_has_http_client, env_set_http_client,
  execute, provide_reqwest_http_client,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn env_with_client(client: ReqwestHttpClient) -> Env {
  let mut env = build_env([provide_reqwest_http_client()]).expect("providers");
  env_set_http_client(&mut env, Arc::new(client) as Arc<dyn HttpClient>);
  env
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

  let env = env_with_client(ReqwestHttpClient::default_client());
  let base = server.uri();

  let put_req = HttpRequest {
    method: HttpMethod::Put,
    url: format!("{base}/p"),
    headers: vec![],
    body: Some(b"u".to_vec()),
    timeout: None,
    max_body_bytes: None,
  };
  let r = run_async(execute(put_req), env.clone()).await.expect("put");
  assert_eq!(r.status, 204);

  let patch_req = HttpRequest {
    method: HttpMethod::Patch,
    url: format!("{base}/a"),
    headers: vec![],
    body: Some(b"x".to_vec()),
    timeout: None,
    max_body_bytes: None,
  };
  let r = run_async(execute(patch_req), env.clone())
    .await
    .expect("patch");
  assert_eq!(r.status, 204);

  let del_req = HttpRequest {
    method: HttpMethod::Delete,
    url: format!("{base}/d"),
    headers: vec![],
    body: None,
    timeout: None,
    max_body_bytes: None,
  };
  let r = run_async(execute(del_req), env).await.expect("delete");
  assert_eq!(r.status, 204);
}

#[tokio::test]
async fn execute_rejects_invalid_header_name() {
  let server = MockServer::start().await;
  let env = env_with_client(ReqwestHttpClient::default_client());
  let req = HttpRequest {
    method: HttpMethod::Get,
    url: format!("{}/n", server.uri()),
    headers: vec![("not a token name\n".to_string(), "v".to_string())],
    body: None,
    timeout: None,
    max_body_bytes: None,
  };
  let err = run_async(execute(req), env)
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

  let env = env_with_client(ReqwestHttpClient::default_client());
  let req = HttpRequest {
    method: HttpMethod::Get,
    url: format!("{}/big", server.uri()),
    headers: vec![],
    body: None,
    timeout: None,
    max_body_bytes: Some(8),
  };
  let err = run_async(execute(req), env).await.expect_err("body cap");
  assert!(matches!(err, HttpError::BodyTooLarge { .. }));
}

#[tokio::test]
async fn reqwest_http_client_provider_builds_env() {
  let env = build_env([provide_reqwest_http_client()]).expect("providers");
  assert!(env_has_http_client(&env));
}

#[tokio::test]
async fn execute_rejects_invalid_header_value() {
  let server = MockServer::start().await;
  let env = env_with_client(ReqwestHttpClient::default_client());
  let req = HttpRequest {
    method: HttpMethod::Get,
    url: format!("{}/v", server.uri()),
    headers: vec![("X-Ok".to_string(), "bad\nvalue".to_string())],
    body: None,
    timeout: None,
    max_body_bytes: None,
  };
  let err = run_async(execute(req), env)
    .await
    .expect_err("header value");
  assert!(matches!(err, HttpError::InvalidRequest(_)));
}

#[tokio::test]
async fn execute_connection_error_surfaces_as_reqwest() {
  let env = env_with_client(ReqwestHttpClient::default_client());
  let req = HttpRequest::get("http://127.0.0.1:1/connection-refused");
  let err = run_async(execute(req), env).await.expect_err("refused");
  assert!(matches!(err, HttpError::Reqwest(_)));
}
