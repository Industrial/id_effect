//! One-shot OpenAI chat completion (requires `OPENAI_API_KEY`).

use id_effect::{build_env, run_async};
use id_effect_ai::{
  AiConfig, ChatMessage, ChatRequest, ChatRole, complete, provide_openai_language_model,
};
use id_effect_platform::http::{ReqwestHttpClient, provide_reqwest_http_client};
use std::sync::Arc;

#[tokio::main]
async fn main() {
  let cfg = AiConfig::from_env();
  let client = Arc::new(ReqwestHttpClient::default_client());
  let env = build_env([
    provide_reqwest_http_client(),
    provide_openai_language_model(client, cfg).expect("openai provider"),
  ])
  .expect("env");
  let req = ChatRequest {
    model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
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
