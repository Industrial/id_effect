//! The reqwest layer's client defaults: a flaky endpoint answers `503` twice, and the default
//! retry policy (configured once on the layer) gets the `200` on the third attempt.
//!
//! Run: `cargo run -p id_effect_platform --example 040_http_client_retry`

use std::time::Duration;

use id_effect::Exit;
use id_effect_platform::http::{
  HttpClientConfig, HttpClientEnv, HttpRequest, ReqwestHttpClientProvider, RetryPolicy, execute,
};
use id_effect_tokio::TokioRuntime;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::main]
async fn main() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/flaky"))
    .respond_with(ResponseTemplate::new(200).set_body_string("third time lucky"))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/flaky"))
    .respond_with(ResponseTemplate::new(503))
    .up_to_n_times(2)
    .with_priority(1)
    .mount(&server)
    .await;

  // Connect 10 s / total 5 s, 3 attempts from 50 ms; GET, PUT and DELETE are retried. The SSRF
  // guard refuses loopback, so the local mock server is allowlisted.
  let config = HttpClientConfig::default()
    .allow_hosts(["127.0.0.1"])
    .with_timeout(Duration::from_secs(5))
    .with_retry(RetryPolicy::exponential(3, Duration::from_millis(50)));
  let program = execute::<HttpClientEnv, _>(HttpRequest::get(format!("{}/flaky", server.uri())))
    .provide_layer(ReqwestHttpClientProvider::layer_with(config));

  let rt = TokioRuntime::current().expect("tokio runtime");
  let Exit::Success(res) = rt.run(program).await else {
    panic!("request failed")
  };
  assert_eq!(res.status, 200);
  assert_eq!(res.body, b"third time lucky");
  println!("040_http_client_retry ok");
}
