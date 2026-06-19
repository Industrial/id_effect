use id_effect::{build_env, run_async};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpRequest, execute, provide_reqwest_http_client, response_body_chunk,
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

  let env = build_env([provide_reqwest_http_client()]).expect("providers");
  let url = format!("{}/hello", server.uri());
  let res = run_async(execute(HttpRequest::get(url)), env)
    .await
    .expect("http ok");
  assert_eq!(res.status, 200);
  assert_eq!(res.body, b"ok");
  let chunk = response_body_chunk(&res);
  assert_eq!(chunk.len(), 2);
}

#[test]
fn http_error_display() {
  let e = HttpError::InvalidRequest("x".into());
  assert!(format!("{e}").contains("invalid"));
}
