//! Deterministic AI with `testing::ScriptedModel` (feature `test-util`): no API key, no network.
//!
//! The program only knows the `LanguageModel` capability; the scripted layer answers in order,
//! streams included, and records what was asked.
//!
//! Run: `cargo run -p id_effect_ai --features test-util --example 005_scripted_model`

use id_effect::{Effect, HCons, HNil};
use id_effect_ai::testing::ScriptedModel;
use id_effect_ai::{AiError, ChatRequest, LanguageModelService, complete, complete_stream};

type Env = HCons<LanguageModelService, HNil>;

/// Classify a document, then stream a summary of it.
fn triage(doc: &str) -> Effect<(String, String), AiError, Env> {
  let summary_req = ChatRequest::user("any-model", format!("Summarize: {doc}"));
  complete::<Env, _>(ChatRequest::user("any-model", format!("Classify: {doc}"))).flat_map(
    move |label| {
      complete_stream::<Env, _>(summary_req)
        .flat_map(|stream| stream.run_collect().narrow())
        .map(move |chunks| {
          let summary: String = chunks.iter().map(|c| c.delta.as_str()).collect();
          (label.content, summary)
        })
    },
  )
}

fn main() {
  let model = ScriptedModel::new()
    .reply("invoice")
    .stream(["An invoice ", "for 3 items."]);
  let (label, summary) = triage("INV-7: 3 items")
    .provide_layer(model.layer::<AiError>())
    .block_on()
    .into_result()
    .expect("scripted run");
  println!("label: {label}\nsummary: {summary}");
  println!("requests: {}", model.requests().len());
  assert_eq!(label, "invoice");
  assert_eq!(summary, "An invoice for 3 items.");
}
