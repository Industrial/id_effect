//! One-shot Anthropic Messages completion, streamed (requires `ANTHROPIC_API_KEY`).

use id_effect::{HCons, HNil};
use id_effect_ai::{
  AiConfig, AiError, ChatRequest, LanguageModelService, complete_stream,
  provide_anthropic_language_model,
};

fn main() {
  let cfg = AiConfig::from_env();
  let layer = provide_anthropic_language_model::<AiError>(cfg.http_client(), cfg)
    .expect("anthropic provider");
  let model =
    std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-20250514".into());
  let prompt = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "Say hello in one word.".into());
  let program =
    complete_stream::<HCons<LanguageModelService, HNil>, _>(ChatRequest::user(model, prompt))
      .provide_layer(layer)
      .flat_map(|stream| stream.run_collect());
  let chunks = program.block_on().into_result().expect("stream");
  let text: String = chunks.iter().map(|c| c.delta.as_str()).collect();
  println!("{text}");
}
