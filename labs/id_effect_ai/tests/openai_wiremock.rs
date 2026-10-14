#![cfg(feature = "openai")]

use id_effect::{build_env, run_async};
use id_effect_ai::{
  AiConfig, ChatMessage, ChatRequest, ChatRole, LanguageModel, OpenAiLanguageModel, complete,
  provide_openai_language_model,
};
use id_effect_config::Secret;
use id_effect_platform::http::provide_reqwest_http_client;
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

  let client = Arc::new(id_effect_platform::http::ReqwestHttpClient::default_client());
  let cfg = test_config(&server.uri());
  let env = build_env([
    provide_reqwest_http_client(),
    provide_openai_language_model(client, cfg).expect("provider"),
  ])
  .expect("env");

  let req = ChatRequest {
    model: "gpt-4o".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hi".into(),
    }],
  };
  let resp = run_async(complete(req), env).await.expect("complete");
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

  let client = Arc::new(id_effect_platform::http::ReqwestHttpClient::default_client());
  let cfg = test_config(&server.uri());
  let model = OpenAiLanguageModel::new(client, &cfg).expect("model");
  let req = ChatRequest {
    model: "gpt-4o".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hi".into(),
    }],
  };
  let err = run_async(model.complete(req), ()).await.unwrap_err();
  assert_eq!(err, id_effect_ai::AiError::Unauthorized);
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

  let client = Arc::new(id_effect_platform::http::ReqwestHttpClient::default_client());
  let cfg = test_config(&server.uri());
  let model = OpenAiLanguageModel::new(client, &cfg).expect("model");
  let req = ChatRequest {
    model: "gpt-4o".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hi".into(),
    }],
  };
  let resp = run_async(model.complete(req), ()).await.expect("retry ok");
  assert_eq!(resp.content, "ok");
}
