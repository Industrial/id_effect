//! Anthropic adapter against a wiremock server (no live vendor call, D8).
#![cfg(feature = "anthropic")]

use std::sync::Arc;

use id_effect::{Effect, Exit, HCons, HNil};
use id_effect_ai::{
  AiConfig, AiError, AnthropicLanguageModel, ChatMessage, ChatRequest, LanguageModel,
  LanguageModelService, Usage, complete, provide_anthropic_language_model,
};
use id_effect_platform::http::HttpClientConfig;
use id_effect_tokio::TokioRuntime;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The config for `server`, with the local mock server (127.0.0.1) allowed by the SSRF policy.
fn test_config(server_uri: &str) -> AiConfig {
  AiConfig::default()
    .with_anthropic_api_key("ant-key")
    .with_anthropic_base_url(server_uri)
    .with_http(HttpClientConfig::default().allow_hosts(["127.0.0.1"]))
}

async fn run<A: Send + 'static>(eff: Effect<A, AiError>) -> Exit<A, AiError> {
  TokioRuntime::current().expect("tokio").run(eff).await
}

fn hi() -> ChatRequest {
  ChatRequest::user("claude-sonnet-4-20250514", "hi")
}

#[tokio::test]
async fn anthropic_complete_success() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/messages"))
    .and(header("x-api-key", "ant-key"))
    .and(header("anthropic-version", "2023-06-01"))
    .and(body_partial_json(serde_json::json!({
      "system": "be brief",
      "messages": [{"role": "user", "content": "hi"}]
    })))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "content": [{"type": "text", "text": "claude says hi"}],
      "usage": {"input_tokens": 3, "output_tokens": 4},
      "stop_reason": "end_turn"
    })))
    .mount(&server)
    .await;

  let cfg = test_config(&server.uri());
  let layer =
    provide_anthropic_language_model::<AiError>(cfg.http_client(), cfg).expect("provider");
  let req = ChatRequest::new(
    "claude-sonnet-4-20250514",
    vec![ChatMessage::system("be brief"), ChatMessage::user("hi")],
  );
  let program = complete::<HCons<LanguageModelService, HNil>, _>(req).provide_layer(layer);
  let resp = run(program).await.into_result().expect("complete");
  assert_eq!(resp.content, "claude says hi");
  assert_eq!(resp.usage, Usage::new(3, 4));
  assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
}

#[tokio::test]
async fn anthropic_stream_parses_sse() {
  let server = MockServer::start().await;
  let body = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n",
    "event: ping\ndata: {\"type\":\"ping\"}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"yo\"}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
  );
  Mock::given(method("POST"))
    .and(path("/v1/messages"))
    .and(body_partial_json(serde_json::json!({"stream": true})))
    .respond_with(
      ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body),
    )
    .mount(&server)
    .await;

  let model = AnthropicLanguageModel::from_config(&test_config(&server.uri())).expect("model");
  let stream = run(model.complete_stream(hi()))
    .await
    .into_result()
    .expect("stream");
  let chunks = run(stream.run_collect())
    .await
    .into_result()
    .expect("chunks");
  let text: String = chunks.iter().map(|c| c.delta.clone()).collect();
  assert_eq!(text, "yo");
  assert!(chunks.last().expect("done").done);
}

#[tokio::test]
async fn anthropic_stream_error_event_fails_the_stream() {
  let server = MockServer::start().await;
  let body = concat!(
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"a\"}}\n\n",
    "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
  );
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(200).set_body_string(body))
    .mount(&server)
    .await;
  let model = AnthropicLanguageModel::from_config(&test_config(&server.uri())).expect("model");
  let stream = run(model.complete_stream(hi()))
    .await
    .into_result()
    .expect("stream");
  let err = run(stream.run_collect()).await.into_result().unwrap_err();
  assert!(matches!(err.fail_option(), Some(AiError::Upstream(m)) if m.contains("Overloaded")));
}

#[tokio::test]
async fn anthropic_transient_status_is_retried() {
  let server = MockServer::start().await;
  // 529 is Anthropic's "overloaded".
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(529))
    .up_to_n_times(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "content": [{"type": "text", "text": "back"}]
    })))
    .mount(&server)
    .await;
  let model = AnthropicLanguageModel::from_config(&test_config(&server.uri())).expect("model");
  let resp = run(model.complete(hi()))
    .await
    .into_result()
    .expect("retried");
  assert_eq!(resp.content, "back");
}

#[tokio::test]
async fn anthropic_bad_request_is_not_retried() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(400).set_body_string("bad model"))
    .expect(1)
    .mount(&server)
    .await;
  let model = AnthropicLanguageModel::from_config(&test_config(&server.uri())).expect("model");
  let err = run(model.complete(hi())).await.into_result().unwrap_err();
  assert!(matches!(err.fail_option(), Some(AiError::Upstream(m)) if m.contains("bad model")));
}

#[test]
fn anthropic_requires_its_key() {
  assert!(AnthropicLanguageModel::from_config(&AiConfig::default()).is_err());
  let client = Arc::new(id_effect_platform::http::ReqwestHttpClient::default_client());
  assert!(provide_anthropic_language_model::<AiError>(client, AiConfig::default()).is_err());
}
