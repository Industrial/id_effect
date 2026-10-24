//! OpenAI adapter against a wiremock server (no live vendor call, D8).
#![cfg(feature = "openai")]

use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, Exit, HCons, HNil, Stream};
use id_effect_ai::{
  AiConfig, AiError, AiLimits, ChatMessage, ChatRequest, CompletionChunk, LanguageModel,
  LanguageModelService, OpenAiLanguageModel, Usage, complete, provide_openai_language_model,
};
use id_effect_platform::http::HttpClientConfig;
use id_effect_tokio::TokioRuntime;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "test-key";

/// The config for `server`, with the local mock server (127.0.0.1) allowed by the SSRF policy.
fn test_config(server_uri: &str) -> AiConfig {
  AiConfig::default()
    .with_openai_api_key(KEY)
    .with_openai_base_url(server_uri)
    .with_http(HttpClientConfig::default().allow_hosts(["127.0.0.1"]))
}

fn model(server: &MockServer) -> OpenAiLanguageModel {
  OpenAiLanguageModel::from_config(&test_config(&server.uri())).expect("model")
}

async fn run<A: Send + 'static>(eff: Effect<A, AiError>) -> Exit<A, AiError> {
  TokioRuntime::current().expect("tokio").run(eff).await
}

async fn collect(
  stream: Stream<CompletionChunk, AiError>,
) -> Result<Vec<CompletionChunk>, AiError> {
  run(stream.run_collect())
    .await
    .into_result()
    .map_err(|c| c.fail_option().expect("typed failure"))
}

fn hi() -> ChatRequest {
  ChatRequest::user("gpt-4o", "hi")
}

#[tokio::test]
async fn openai_complete_success() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/chat/completions"))
    .and(header("Authorization", format!("Bearer {KEY}").as_str()))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "choices": [{"message": {"content": "hello"}, "finish_reason": "stop"}],
      "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3}
    })))
    .mount(&server)
    .await;

  let client = test_config(&server.uri()).http_client();
  let layer =
    provide_openai_language_model::<AiError>(client, test_config(&server.uri())).expect("provider");
  let program = complete::<HCons<LanguageModelService, HNil>, _>(hi()).provide_layer(layer);
  let resp = run(program).await.into_result().expect("complete");
  assert_eq!(resp.content, "hello");
  assert_eq!(resp.usage, Usage::new(1, 2));
  assert_eq!(resp.stop_reason.as_deref(), Some("stop"));
}

#[tokio::test]
async fn openai_unauthorized_no_retry() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/chat/completions"))
    .respond_with(ResponseTemplate::new(401).set_body_string("bad"))
    .expect(1)
    .mount(&server)
    .await;

  let err = run(model(&server).complete(hi()))
    .await
    .into_result()
    .unwrap_err();
  assert_eq!(err.fail_option(), Some(AiError::Unauthorized));
}

#[tokio::test]
async fn openai_retries_transient_then_succeeds() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/chat/completions"))
    .respond_with(ResponseTemplate::new(429).set_body_string("rate"))
    .up_to_n_times(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/v1/chat/completions"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "choices": [{"message": {"content": "ok"}}],
      "usage": {"total_tokens": 1}
    })))
    .mount(&server)
    .await;

  let resp = run(model(&server).complete(hi()))
    .await
    .into_result()
    .expect("retry ok");
  assert_eq!(resp.content, "ok");
}

#[tokio::test]
async fn openai_upstream_error_detail_is_reported() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(400).set_body_string("model not found"))
    .mount(&server)
    .await;
  let err = run(model(&server).complete(hi()))
    .await
    .into_result()
    .unwrap_err()
    .fail_option();
  assert!(matches!(err, Some(AiError::Upstream(d)) if d.contains("model not found")));
}

#[tokio::test]
async fn openai_stream_parses_sse() {
  let server = MockServer::start().await;
  let body = concat!(
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
    "data: [DONE]\n\n",
  );
  Mock::given(method("POST"))
    .and(path("/v1/chat/completions"))
    .and(body_partial_json(serde_json::json!({"stream": true})))
    .respond_with(
      ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body),
    )
    .mount(&server)
    .await;

  let stream = run(model(&server).complete_stream(hi()))
    .await
    .into_result()
    .expect("stream");
  let chunks = collect(stream).await.expect("chunks");
  let text: String = chunks.iter().map(|c| c.delta.as_str()).collect();
  assert_eq!(text, "hello");
  assert!(chunks.last().expect("done").done);
}

#[tokio::test]
async fn openai_stream_refused_status_is_typed() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(403))
    .mount(&server)
    .await;
  let err = run(model(&server).complete_stream(hi()))
    .await
    .into_result()
    .unwrap_err();
  assert_eq!(err.fail_option(), Some(AiError::Unauthorized));
}

#[tokio::test]
async fn openai_request_timeout_is_typed() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(
      ResponseTemplate::new(200)
        .set_delay(Duration::from_secs(5))
        .set_body_string("{}"),
    )
    .mount(&server)
    .await;
  let cfg = test_config(&server.uri())
    .with_limits(AiLimits::default().with_request_timeout(Duration::from_millis(100)));
  let model = OpenAiLanguageModel::from_config(&cfg).expect("model");
  let err = run(model.complete(hi())).await.into_result().unwrap_err();
  assert_eq!(err.fail_option(), Some(AiError::Timeout));
}

#[tokio::test]
async fn openai_response_over_cap_is_refused() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(4096)))
    .mount(&server)
    .await;
  let cfg =
    test_config(&server.uri()).with_limits(AiLimits::default().with_max_response_bytes(1024));
  let model = OpenAiLanguageModel::from_config(&cfg).expect("model");
  let err = run(model.complete(hi())).await.into_result().unwrap_err();
  assert!(matches!(
    err.fail_option(),
    Some(AiError::LimitExceeded {
      limit: "response_bytes",
      ..
    })
  ));
}

#[tokio::test]
async fn openai_invalid_request_is_not_sent() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(200))
    .expect(0)
    .mount(&server)
    .await;
  let model = model(&server);
  let empty = ChatRequest::new("gpt-4o", Vec::<ChatMessage>::new());
  let err = run(model.complete(empty.clone()))
    .await
    .into_result()
    .unwrap_err();
  assert_eq!(err.fail_option(), Some(AiError::EmptyRequest));
  let err = run(model.complete_stream(empty))
    .await
    .into_result()
    .unwrap_err();
  assert_eq!(err.fail_option(), Some(AiError::EmptyRequest));
}

#[test]
fn openai_requires_its_key() {
  let err = OpenAiLanguageModel::from_config(&AiConfig::default()).unwrap_err();
  assert!(matches!(err, AiError::Upstream(m) if m.contains("OPENAI_API_KEY")));
  let client = Arc::new(id_effect_platform::http::ReqwestHttpClient::default_client());
  assert!(provide_openai_language_model::<AiError>(client, AiConfig::default()).is_err());
}
