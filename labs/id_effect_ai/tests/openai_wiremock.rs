#![cfg(feature = "openai")]

use id_effect::Exit;
use id_effect::{Effect, HCons, HNil};
use id_effect_ai::{
  AiConfig, AiError, ChatMessage, ChatRequest, ChatRole, LanguageModel, LanguageModelService,
  OpenAiLanguageModel, complete, provide_openai_language_model,
};
use id_effect_config::Secret;
use id_effect_tokio::TokioRuntime;
use std::sync::Arc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config(server_uri: &str) -> AiConfig {
  AiConfig {
    openai_api_key: Some(Secret::new("test-key".to_string())),
    openai_base_url: server_uri.to_string(),
    ..AiConfig::default()
  }
}

/// The default client with the local mock server (127.0.0.1) exempt from the SSRF guard.
fn local_client() -> id_effect_platform::http::ReqwestHttpClient {
  let config = id_effect_platform::http::HttpClientConfig::default().allow_hosts(["127.0.0.1"]);
  id_effect_platform::http::ReqwestHttpClient::from_config(&config)
}

async fn run<A: Send + 'static>(eff: Effect<A, AiError>) -> Exit<A, AiError> {
  TokioRuntime::current().expect("tokio").run(eff).await
}

#[tokio::test]
async fn openai_complete_success() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/chat/completions"))
    .and(header("Authorization", "Bearer test-key"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "choices": [{"message": {"content": "hello"}}],
      "usage": {"total_tokens": 3}
    })))
    .mount(&server)
    .await;

  let client = Arc::new(local_client());
  let cfg = test_config(&server.uri());
  let layer = provide_openai_language_model::<AiError>(client, cfg).expect("provider");

  let req = ChatRequest {
    model: "gpt-4o".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hi".into(),
    }],
  };
  let program = complete::<HCons<LanguageModelService, HNil>, _>(req).provide_layer(layer);
  let resp = run(program).await.into_result().expect("complete");
  assert_eq!(resp.content, "hello");
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

  let client = Arc::new(local_client());
  let cfg = test_config(&server.uri());
  let model = OpenAiLanguageModel::new(client, &cfg).expect("model");
  let req = ChatRequest {
    model: "gpt-4o".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hi".into(),
    }],
  };
  let err = run(model.complete(req)).await.into_result().unwrap_err();
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

  let client = Arc::new(local_client());
  let cfg = test_config(&server.uri());
  let model = OpenAiLanguageModel::new(client, &cfg).expect("model");
  let req = ChatRequest {
    model: "gpt-4o".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hi".into(),
    }],
  };
  let resp = run(model.complete(req))
    .await
    .into_result()
    .expect("retry ok");
  assert_eq!(resp.content, "ok");
}
