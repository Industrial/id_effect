//! One-shot OpenAI chat completion (requires `OPENAI_API_KEY`).

use id_effect::next::{HCons, HNil};
use id_effect_ai::{
  AiConfig, AiError, ChatMessage, ChatRequest, ChatRole, LanguageModelService, complete,
  provide_openai_language_model,
};
use id_effect_platform::http::ReqwestHttpClient;
use std::sync::Arc;

fn main() {
  let cfg = AiConfig::from_env();
  let client = Arc::new(ReqwestHttpClient::default_client());
  let layer = provide_openai_language_model::<AiError>(client, cfg).expect("openai provider");
  let req = ChatRequest {
    model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Say hello in one word.".into()),
    }],
  };
  // The program edge: run on the default runtime (its tokio drivers serve reqwest).
  let resp = complete::<HCons<LanguageModelService, HNil>, _>(req)
    .provide_layer(layer)
    .block_on()
    .into_result()
    .expect("complete");
  println!("{}", resp.content);
}
