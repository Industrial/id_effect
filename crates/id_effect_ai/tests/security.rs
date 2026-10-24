//! ADR 0013 rules applied to the AI crate (ADR 0015 B13, B14, R6): secrets redacted, prompts not
//! recorded unless the recorded opt-out is taken, provider base URLs under the platform SSRF
//! policy, a finite `max_tokens` on every request, and streams closed over their caps.
#![cfg(all(feature = "openai", feature = "anthropic"))]

use tokio::sync::{Mutex, MutexGuard};

use id_effect::observability::posture::dangerous_opt_outs;
use id_effect::{
  Effect, Exit, SpanRecord, TracingConfig, install_tracing_layer, snapshot_tracing, tracing_hook,
};
use id_effect_ai::{
  AI_SPAN, AiConfig, AiError, AiLimits, AnthropicLanguageModel, ChatRequest, DANGEROUS_LOG_PROMPTS,
  LanguageModel, OpenAiLanguageModel,
};
use id_effect_platform::http::HttpClientConfig;
use id_effect_tokio::TokioRuntime;
use wiremock::matchers::{any, body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROMPT: &str = "my-secret-prompt";
const COMPLETION: &str = "the-secret-completion";

/// Serializes the tests that share the process-global tracing buffer.
async fn trace_lock() -> MutexGuard<'static, ()> {
  static LOCK: Mutex<()> = Mutex::const_new(());
  LOCK.lock().await
}

fn local(cfg: AiConfig, uri: &str) -> AiConfig {
  cfg
    .with_openai_api_key("sk-openai")
    .with_openai_base_url(uri)
    .with_anthropic_api_key("sk-anthropic")
    .with_anthropic_base_url(uri)
    .with_http(HttpClientConfig::default().allow_hosts(["127.0.0.1"]))
}

async fn run<A: Send + 'static>(eff: Effect<A, AiError>) -> Exit<A, AiError> {
  TokioRuntime::current().expect("tokio").run(eff).await
}

async fn fail_of<A: Send + 'static>(eff: Effect<A, AiError>) -> Option<AiError> {
  run(eff)
    .await
    .into_result()
    .err()
    .and_then(|c| c.fail_option())
}

async fn mount_completions(server: &MockServer) {
  Mock::given(method("POST"))
    .and(path("/v1/chat/completions"))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "choices": [{"message": {"content": COMPLETION}, "finish_reason": "stop"}],
      "usage": {"prompt_tokens": 5, "completion_tokens": 7}
    })))
    .mount(server)
    .await;
}

/// Runs one traced OpenAI completion of [`PROMPT`] and returns the `ai.request` span record.
async fn traced_completion(cfg: AiConfig) -> SpanRecord {
  let model = OpenAiLanguageModel::from_config(&cfg).expect("model");
  install_tracing_layer(TracingConfig::enabled());
  let resp = run(
    model
      .complete(ChatRequest::user("gpt-4o", PROMPT))
      .with_span_hook(tracing_hook()),
  )
  .await
  .into_result()
  .expect("complete");
  assert_eq!(resp.content, COMPLETION);
  let snap = snapshot_tracing();
  install_tracing_layer(TracingConfig::default());
  snap
    .spans
    .into_iter()
    .find(|s| s.name == AI_SPAN)
    .expect("ai.request span")
}

#[test]
fn ai_api_key_redacted_in_debug() {
  let cfg = AiConfig::default()
    .with_openai_api_key("sk-openai-secret")
    .with_anthropic_api_key("sk-anthropic-secret")
    .with_cursor_api_key("cursor-secret");
  let client = cfg.http_client();
  let openai = OpenAiLanguageModel::new(client.clone(), &cfg).expect("openai");
  let anthropic = AnthropicLanguageModel::new(client, &cfg).expect("anthropic");
  for printed in [
    format!("{cfg:?}"),
    format!("{openai:?}"),
    format!("{anthropic:?}"),
  ] {
    for key in ["sk-openai-secret", "sk-anthropic-secret", "cursor-secret"] {
      assert!(!printed.contains(key), "{key} printed in {printed}");
    }
    assert!(printed.contains("<redacted>"), "{printed}");
  }
}

#[tokio::test]
async fn ai_prompts_not_logged_by_default() {
  let _g = trace_lock().await;
  let server = MockServer::start().await;
  mount_completions(&server).await;
  let span = traced_completion(local(AiConfig::default(), &server.uri())).await;
  for value in span.annotations.values() {
    assert!(!value.contains(PROMPT), "prompt recorded: {value}");
    assert!(!value.contains(COMPLETION), "completion recorded: {value}");
  }
  assert!(!span.annotations.contains_key("ai.prompt"));
  assert!(!span.annotations.contains_key("ai.completion"));
  // The metadata is there: vendor, model, usage.
  assert_eq!(span.annotations["ai.vendor"], "openai");
  assert_eq!(span.annotations["ai.model"], "gpt-4o");
  assert_eq!(span.annotations["ai.usage.output_tokens"], "7");
}

#[tokio::test]
async fn dangerous_log_prompts_is_recorded() {
  let _g = trace_lock().await;
  let cfg = AiConfig::default();
  assert!(!cfg.logs_prompts());
  let cfg = cfg.dangerous_log_prompts();
  assert!(cfg.logs_prompts());
  assert!(
    dangerous_opt_outs()
      .iter()
      .any(|(id, reopens)| *id == DANGEROUS_LOG_PROMPTS && reopens.starts_with("R3")),
    "opt-out not in the posture registry"
  );
  // Taken, it does what it says: the prompt and the completion reach the span.
  let server = MockServer::start().await;
  mount_completions(&server).await;
  let span = traced_completion(local(cfg, &server.uri())).await;
  assert!(span.annotations["ai.prompt"].contains(PROMPT));
  assert_eq!(span.annotations["ai.completion"], COMPLETION);
}

#[tokio::test]
async fn ai_base_url_obeys_ssrf_policy() {
  // A real server on loopback: the default policy must refuse it without sending anything.
  let server = MockServer::start().await;
  Mock::given(any())
    .respond_with(ResponseTemplate::new(200))
    .expect(0)
    .mount(&server)
    .await;
  let loopback = AiConfig::default()
    .with_openai_api_key("k")
    .with_openai_base_url(server.uri())
    .with_anthropic_api_key("k")
    .with_anthropic_base_url(server.uri());
  let openai = OpenAiLanguageModel::from_config(&loopback).expect("openai");
  let anthropic = AnthropicLanguageModel::from_config(&loopback).expect("anthropic");
  let req = ChatRequest::user("m", "hi");
  assert!(matches!(
    fail_of(openai.complete(req.clone())).await,
    Some(AiError::Blocked(_))
  ));
  assert!(matches!(
    fail_of(anthropic.complete_stream(req.clone())).await,
    Some(AiError::Blocked(_))
  ));
  // Cloud metadata and non-HTTP schemes are refused too.
  for base in ["http://169.254.169.254", "file:///etc"] {
    let cfg = loopback.clone().with_openai_base_url(base);
    let model = OpenAiLanguageModel::from_config(&cfg).expect("openai");
    assert!(
      matches!(
        fail_of(model.complete(req.clone())).await,
        Some(AiError::Blocked(_))
      ),
      "{base} not blocked"
    );
  }
}

#[tokio::test]
async fn r4_ai_max_tokens_finite_by_default() {
  assert_eq!(AiLimits::default().max_tokens, AiLimits::DEFAULT_MAX_TOKENS);
  assert_eq!(AiLimits::DEFAULT_MAX_TOKENS, 1024);
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/chat/completions"))
    .and(body_partial_json(
      serde_json::json!({"max_completion_tokens": 1024}),
    ))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "choices": [{"message": {"content": "o"}}]
    })))
    .expect(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/v1/messages"))
    .and(body_partial_json(serde_json::json!({"max_tokens": 1024})))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "content": [{"type": "text", "text": "a"}]
    })))
    .expect(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/v1/messages"))
    .and(body_partial_json(serde_json::json!({"max_tokens": 7})))
    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
      "content": [{"type": "text", "text": "seven"}]
    })))
    .expect(1)
    .mount(&server)
    .await;
  let cfg = local(AiConfig::default(), &server.uri());
  let openai = OpenAiLanguageModel::from_config(&cfg).expect("openai");
  let anthropic = AnthropicLanguageModel::from_config(&cfg).expect("anthropic");
  let req = ChatRequest::user("m", "hi");
  assert!(req.max_tokens.is_none());
  run(openai.complete(req.clone()))
    .await
    .into_result()
    .expect("openai default cap");
  run(anthropic.complete(req.clone()))
    .await
    .into_result()
    .expect("anthropic default cap");
  let resp = run(anthropic.complete(req.with_max_tokens(7)))
    .await
    .into_result()
    .expect("per-request cap");
  assert_eq!(resp.content, "seven");
}

#[tokio::test]
async fn r4_ai_stream_over_cap_closed() {
  let server = MockServer::start().await;
  let event = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
  Mock::given(method("POST"))
    .and(path("/v1/chat/completions"))
    .respond_with(
      ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(event.repeat(1000)),
    )
    .mount(&server)
    .await;
  let req = ChatRequest::user("m", "hi");
  let cases = [
    (
      AiLimits::default().with_max_stream_events(5),
      "stream_events",
    ),
    (
      AiLimits::default().with_max_stream_bytes(256),
      "stream_bytes",
    ),
  ];
  for (limits, name) in cases {
    let cfg = local(AiConfig::default(), &server.uri()).with_limits(limits);
    let model = OpenAiLanguageModel::from_config(&cfg).expect("openai");
    let stream = run(model.complete_stream(req.clone()))
      .await
      .into_result()
      .expect("stream opens");
    let err = fail_of(stream.run_collect()).await;
    assert!(
      matches!(err, Some(AiError::LimitExceeded { limit, .. }) if limit == name),
      "{name}: {err:?}"
    );
  }
  // Under the caps the same stream completes.
  let cfg = local(AiConfig::default(), &server.uri());
  let model = OpenAiLanguageModel::from_config(&cfg).expect("openai");
  let stream = run(model.complete_stream(req))
    .await
    .into_result()
    .expect("stream opens");
  let chunks = run(stream.run_collect())
    .await
    .into_result()
    .expect("within caps");
  assert_eq!(chunks.len(), 1001);
}
