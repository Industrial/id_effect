//! One-shot Anthropic chat completion (requires `ANTHROPIC_API_KEY`).

use id_effect::{build_env, run_async};
use id_effect_ai::{
  AiConfig, ChatMessage, ChatRequest, ChatRole, complete, provide_anthropic_language_model,
};
use id_effect_platform::http::{ReqwestHttpClient, provide_reqwest_http_client};
use std::sync::Arc;

#[tokio::main]
async fn main() {
  let cfg = AiConfig::from_env();
  let client = Arc::new(ReqwestHttpClient::default_client());
  let env = build_env([
    provide_reqwest_http_client(),
    provide_anthropic_language_model(client, cfg).expect("anthropic provider"),
  ])
  .expect("env");
  let req = ChatRequest {
    model: std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-20250514".into()),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Say hello in one word.".into()),
    }],
  };
  let resp = run_async(complete(req), env).await.expect("complete");
  println!("{}", resp.content);
}
