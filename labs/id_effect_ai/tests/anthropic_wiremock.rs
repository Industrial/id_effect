#![cfg(feature = "anthropic")]

use id_effect::Exit;
use id_effect::next::{Effect, HCons, HNil};
use id_effect_ai::{
  AiConfig, AiError, AnthropicLanguageModel, ChatMessage, ChatRequest, ChatRole, LanguageModel,
  LanguageModelService, complete, provide_anthropic_language_model,
};
use id_effect_config::Secret;
use id_effect_tokio::TokioRuntime;
use std::sync::Arc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config(server_uri: &str) -> AiConfig {
  AiConfig {
    anthropic_api_key: Some(Secret::new("ant-key".to_string())),
    anthropic_base_url: server_uri.to_string(),
    ..AiConfig::default()
  }
}

async fn run<A: Send + 'static>(eff: Effect<A, AiError>) -> Exit<A, AiError> {
  TokioRuntime::current().expect("tokio").run(eff).await
}

#[tokio::test]
async fn anthropic_complete_success() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/messages"))
    .and(header("x-api-key", "ant-key"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "content": [{"type": "text", "text": "claude says hi"}],
      "usage": {"output_tokens": 4}
    })))
    .mount(&server)
    .await;

  let client = Arc::new(id_effect_platform::http::ReqwestHttpClient::default_client());
  let cfg = test_config(&server.uri());
  let layer = provide_anthropic_language_model::<AiError>(client, cfg).expect("provider");

  let req = ChatRequest {
    model: "claude-sonnet-4-20250514".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hi".into(),
    }],
  };
  let program = complete::<HCons<LanguageModelService, HNil>, _>(req).provide_layer(layer);
  let resp = run(program).await.into_result().expect("complete");
  assert_eq!(resp.content, "claude says hi");
}

#[tokio::test]
async fn anthropic_stream_parses_sse() {
  let server = MockServer::start().await;
  let body = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"yo\"}}\n\n";
  Mock::given(method("POST"))
    .and(path("/v1/messages"))
    .respond_with(
      ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body),
    )
    .mount(&server)
    .await;

  let client = Arc::new(id_effect_platform::http::ReqwestHttpClient::default_client());
  let cfg = test_config(&server.uri());
  let model = AnthropicLanguageModel::new(client, &cfg).expect("model");
  let req = ChatRequest {
    model: "claude-sonnet-4-20250514".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hi".into(),
    }],
  };
  let stream = run(model.complete_stream(req))
    .await
    .into_result()
    .expect("stream");
  let chunks = run(stream.run_collect())
    .await
    .into_result()
    .expect("chunks");
  let text: String = chunks.iter().map(|c| c.delta.clone()).collect();
  assert!(text.contains("yo"));
}
