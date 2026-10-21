//! ai.llm #7: every vendor call runs in an `ai.request` span with vendor, operation, model and
//! token usage.
#![cfg(all(feature = "openai", feature = "anthropic"))]

use id_effect::{
  EffectEvent, TracingConfig, install_tracing_layer, snapshot_tracing, tracing_hook,
};
use id_effect_ai::{
  AI_SPAN, AiConfig, AiError, AnthropicLanguageModel, ChatRequest, LanguageModel, with_ai_span,
};
use id_effect_platform::http::HttpClientConfig;
use id_effect_tokio::TokioRuntime;
use tokio::sync::Mutex;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

static TRACE: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn ai_request_span_records_vendor_model_and_usage() {
  let _g = TRACE.lock().await;
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "content": [{"type": "text", "text": "ok"}],
      "usage": {"input_tokens": 11, "output_tokens": 13},
      "stop_reason": "max_tokens"
    })))
    .mount(&server)
    .await;
  let cfg = AiConfig::default()
    .with_anthropic_api_key("k")
    .with_anthropic_base_url(server.uri())
    .with_http(HttpClientConfig::default().allow_hosts(["127.0.0.1"]));
  let model = AnthropicLanguageModel::from_config(&cfg).expect("model");
  install_tracing_layer(TracingConfig::enabled());
  TokioRuntime::current()
    .expect("tokio")
    .run(
      model
        .complete(ChatRequest::user("claude-x", "hi"))
        .with_span_hook(tracing_hook()),
    )
    .await
    .into_result()
    .expect("complete");
  let snap = snapshot_tracing();
  install_tracing_layer(TracingConfig::default());
  let span = snap.spans.iter().find(|s| s.name == AI_SPAN).expect("span");
  let get = |k: &str| span.annotations.get(k).map(String::as_str);
  assert_eq!(get("ai.vendor"), Some("anthropic"));
  assert_eq!(get("ai.operation"), Some("chat"));
  assert_eq!(get("ai.model"), Some("claude-x"));
  assert_eq!(get("ai.usage.input_tokens"), Some("11"));
  assert_eq!(get("ai.usage.output_tokens"), Some("13"));
  assert_eq!(get("ai.stop_reason"), Some("max_tokens"));
  assert!(snap.effect_events.contains(&EffectEvent::Success {
    span: AI_SPAN.into()
  }));
}

#[tokio::test]
async fn with_ai_span_wraps_any_effect() {
  let _g = TRACE.lock().await;
  install_tracing_layer(TracingConfig::enabled());
  let eff = with_ai_span(
    "custom",
    "embed",
    "m",
    id_effect::fail::<u8, AiError, id_effect::HNil>(AiError::Timeout),
  );
  let exit = TokioRuntime::current()
    .expect("tokio")
    .run(eff.with_span_hook(tracing_hook()))
    .await;
  assert!(exit.into_result().is_err());
  let snap = snapshot_tracing();
  install_tracing_layer(TracingConfig::default());
  let span = snap.spans.iter().find(|s| s.name == AI_SPAN).expect("span");
  assert_eq!(span.annotations["ai.vendor"], "custom");
  assert!(snap.effect_events.contains(&EffectEvent::Failure {
    span: AI_SPAN.into()
  }));
}
