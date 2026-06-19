//! Streaming HTTP body tests for [`ReqwestHttpClient::execute_stream`].

use id_effect::{build_env, run_async};
use id_effect_platform::http::{HttpRequest, execute_stream, provide_reqwest_http_client};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn execute_stream_reads_chunked_body() {
  let server = MockServer::start().await;
  let body = vec![b'a'; 100];
  Mock::given(method("GET"))
    .and(path("/stream"))
    .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
    .mount(&server)
    .await;

  let env = build_env([provide_reqwest_http_client()]).expect("providers");
  let url = format!("{}/stream", server.uri());
  let streamed = run_async(execute_stream(HttpRequest::get(url)), env)
    .await
    .expect("stream ok");
  assert_eq!(streamed.status, 200);

  let out = run_async(streamed.body.run_collect(), ())
    .await
    .expect("collect");
  assert_eq!(out, body);
}

#[tokio::test]
async fn execute_stream_rejects_body_larger_than_max() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/big"))
    .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 64]))
    .mount(&server)
    .await;

  let env = build_env([provide_reqwest_http_client()]).expect("providers");
  let req = HttpRequest {
    method: id_effect_platform::http::HttpMethod::Get,
    url: format!("{}/big", server.uri()),
    headers: vec![],
    body: None,
    timeout: None,
    max_body_bytes: Some(8),
  };
  let streamed = run_async(execute_stream(req), env)
    .await
    .expect("headers ok");
  let err = run_async(streamed.body.run_collect(), ())
    .await
    .expect_err("body cap");
  assert!(matches!(
    err,
    id_effect_platform::error::HttpError::BodyTooLarge { .. }
  ));
}
