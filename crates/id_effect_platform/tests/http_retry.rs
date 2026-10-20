//! The reqwest layer's client defaults (D10): timeouts and the client-level retry policy.

mod common;

use std::time::{Duration, Instant};

use common::{local_config, run};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClientConfig, HttpClientEnv, HttpMethod, HttpRequest, ReqwestHttpClientProvider, RetryPolicy,
  execute,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The default policy with a 1 ms base delay (same methods and attempts, fast tests).
fn fast_config() -> HttpClientConfig {
  local_config().with_retry(RetryPolicy::exponential(
    RetryPolicy::DEFAULT_MAX_ATTEMPTS,
    Duration::from_millis(1),
  ))
}

async fn requests(server: &MockServer) -> usize {
  server.received_requests().await.map_or(0, |r| r.len())
}

#[tokio::test]
async fn retries_idempotent_get_then_succeeds() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/flaky"))
    .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/flaky"))
    .respond_with(ResponseTemplate::new(503))
    .up_to_n_times(2)
    .with_priority(1)
    .mount(&server)
    .await;

  let program = execute::<HttpClientEnv, _>(HttpRequest::get(format!("{}/flaky", server.uri())))
    .provide_layer(ReqwestHttpClientProvider::layer_with(fast_config()));
  let res = run(program).await.expect("http ok");
  assert_eq!((res.status, res.body.as_slice()), (200, &b"ok"[..]));
  assert_eq!(requests(&server).await, 3);
}

#[tokio::test]
async fn post_not_retried_by_default() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/submit"))
    .respond_with(ResponseTemplate::new(503))
    .mount(&server)
    .await;
  let url = format!("{}/submit", server.uri());

  let program = execute::<HttpClientEnv, _>(HttpRequest::post(url.clone(), b"x".to_vec()))
    .provide_layer(ReqwestHttpClientProvider::layer_with(local_config()));
  assert_eq!(run(program).await.expect("http ok").status, 503);
  assert_eq!(requests(&server).await, 1);

  // Opted in once on the layer, POST is retried like the idempotent methods.
  let config = fast_config().with_retry(
    RetryPolicy::exponential(3, Duration::from_millis(1)).with_methods([HttpMethod::Post]),
  );
  let program = execute::<HttpClientEnv, _>(HttpRequest::post(url, b"x".to_vec()))
    .provide_layer(ReqwestHttpClientProvider::layer_with(config));
  assert_eq!(run(program).await.expect("http ok").status, 503);
  assert_eq!(requests(&server).await, 1 + 3);
}

#[tokio::test]
async fn retry_after_is_honoured() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/limited"))
    .respond_with(ResponseTemplate::new(200))
    .mount(&server)
    .await;
  Mock::given(method("GET"))
    .and(path("/limited"))
    .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
    .up_to_n_times(1)
    .with_priority(1)
    .mount(&server)
    .await;
  let url = format!("{}/limited", server.uri());

  let start = Instant::now();
  let program = execute::<HttpClientEnv, _>(HttpRequest::get(url.clone()))
    .provide_layer(ReqwestHttpClientProvider::layer_with(fast_config()));
  assert_eq!(run(program).await.expect("http ok").status, 200);
  assert!(
    start.elapsed() >= Duration::from_secs(1),
    "{:?}",
    start.elapsed()
  );
  assert_eq!(requests(&server).await, 2);

  // A Retry-After beyond the policy's cap ends the retries with that response.
  server.reset().await;
  Mock::given(method("GET"))
    .and(path("/limited"))
    .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "120"))
    .mount(&server)
    .await;
  let program = execute::<HttpClientEnv, _>(HttpRequest::get(url))
    .provide_layer(ReqwestHttpClientProvider::layer_with(fast_config()));
  assert_eq!(run(program).await.expect("http ok").status, 429);
  assert_eq!(requests(&server).await, 1);
}

#[tokio::test]
async fn default_timeout_applies() {
  let defaults = HttpClientConfig::default();
  assert_eq!(defaults.connect_timeout, Duration::from_secs(10));
  assert_eq!(defaults.timeout, Duration::from_secs(30));

  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/slow"))
    .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
    .mount(&server)
    .await;

  // The client-wide timeout applies to a request that sets none, and a timeout is not retried.
  let config = fast_config().with_timeout(Duration::from_millis(200));
  let start = Instant::now();
  let program = execute::<HttpClientEnv, _>(HttpRequest::get(format!("{}/slow", server.uri())))
    .provide_layer(ReqwestHttpClientProvider::layer_with(config));
  let err = run(program).await.expect_err("timed out");
  assert!(
    matches!(&err, HttpError::Reqwest(e) if e.is_timeout()),
    "{err:?}"
  );
  assert!(start.elapsed() < Duration::from_secs(5));
  assert_eq!(requests(&server).await, 1);
}
