mod common;

use common::{local_config, run};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClientEnv, HttpRequest, ReqwestHttpClientProvider, execute, response_body_stream,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn execute_hits_mock_server() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/hello"))
    .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
    .mount(&server)
    .await;

  let url = format!("{}/hello", server.uri());
  let program = execute::<HttpClientEnv, _>(HttpRequest::get(url))
    .provide_layer(ReqwestHttpClientProvider::layer_with(local_config()));
  let res = run(program).await.expect("http ok");
  assert_eq!(res.status, 200);
  assert_eq!(res.body, b"ok");
  let body = response_body_stream(&res, 1);
  assert_eq!(
    run(body.next_chunk()).await.expect("chunk"),
    Some(b"o".to_vec())
  );
}

#[test]
fn http_error_display() {
  let e = HttpError::InvalidRequest("x".into());
  assert!(format!("{e}").contains("invalid"));
}
