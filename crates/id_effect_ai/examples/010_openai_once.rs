//! One-shot OpenAI chat completion (requires `OPENAI_API_KEY`).
//!
//! The model is built from `AiConfig::from_env`, so the request goes through the platform HTTP
//! client with its SSRF policy and timeouts, and carries the default `max_tokens` cap.

use id_effect::{HCons, HNil};
use id_effect_ai::{
  AiConfig, AiError, ChatRequest, LanguageModelService, complete, provide_openai_language_model,
};

fn main() {
  let cfg = AiConfig::from_env();
  let layer =
    provide_openai_language_model::<AiError>(cfg.http_client(), cfg).expect("openai provider");
  let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
  let prompt = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "Say hello in one word.".into());
  // The program edge: run on the default runtime (its tokio drivers serve reqwest).
  let resp = complete::<HCons<LanguageModelService, HNil>, _>(ChatRequest::user(model, prompt))
    .provide_layer(layer)
    .block_on()
    .into_result()
    .expect("complete");
  println!("{}", resp.content);
}
