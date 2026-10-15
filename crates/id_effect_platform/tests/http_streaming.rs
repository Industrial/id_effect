//! Streaming HTTP body tests for [`ReqwestHttpClient::execute_stream`].

mod common;

use common::run;
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClientEnv, HttpMethod, HttpRequest, execute_stream, provide_reqwest_http_client,
};
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

  let url = format!("{}/stream", server.uri());
  let program = execute_stream::<HttpClientEnv, _>(HttpRequest::get(url))
    .provide_layer(provide_reqwest_http_client());
  let streamed = run(program).await.expect("stream ok");
  assert_eq!(streamed.status, 200);

  let out = run(streamed.body.collect()).await.expect("collect");
  assert_eq!(out, body);
}

#[tokio::test]
async fn execute_stream_reads_chunk_by_chunk() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/pull"))
    .respond_with(ResponseTemplate::new(200).set_body_bytes(b"pulled".to_vec()))
    .mount(&server)
    .await;

  let url = format!("{}/pull", server.uri());
  let program = execute_stream::<HttpClientEnv, _>(HttpRequest::get(url))
    .provide_layer(provide_reqwest_http_client());
  let streamed = run(program).await.expect("stream ok");
  let mut got = Vec::new();
  while let Some(chunk) = run(streamed.body.next_chunk()).await.expect("chunk") {
    got.extend(chunk);
  }
  assert_eq!(got, b"pulled");
  assert_eq!(run(streamed.body.next_chunk()).await.expect("end"), None);
}

#[tokio::test]
async fn execute_stream_rejects_body_larger_than_max() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/big"))
    .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 64]))
    .mount(&server)
    .await;

  let req = HttpRequest {
    method: HttpMethod::Get,
    url: format!("{}/big", server.uri()),
    headers: vec![],
    body: None,
    timeout: None,
    max_body_bytes: Some(8),
  };
  let program =
    execute_stream::<HttpClientEnv, _>(req).provide_layer(provide_reqwest_http_client());
  let streamed = run(program).await.expect("headers ok");
  let err = run(streamed.body.collect()).await.expect_err("body cap");
  assert!(matches!(err, HttpError::BodyTooLarge { .. }));
}
