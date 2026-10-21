//! The span of an embedding vendor call (ai.llm #7, as for chat): `ai.request` with
//! `ai.operation = "embed"`, annotated with counts and usage, never with input text (R3).

use id_effect::{Effect, annotate_current_span, succeed};

use super::{EmbedRequest, EmbedResponse, EmbeddingError};
use crate::tracing_util::with_ai_request_span;

/// `call` inside the vendor's `ai.request` span, annotated on success with
/// `ai.embedding.inputs`, `ai.embedding.dimensions` and `ai.usage.input_tokens`. The input
/// texts are never recorded, whatever the config says about prompts.
pub(crate) fn observe_embed(
  vendor: &'static str,
  req: &EmbedRequest,
  call: Effect<EmbedResponse, EmbeddingError>,
) -> Effect<EmbedResponse, EmbeddingError> {
  let inputs = req.inputs.len();
  let effect = call.flat_map(move |resp| {
    let notes = [
      ("ai.embedding.inputs", inputs.to_string()),
      (
        "ai.embedding.dimensions",
        resp.dimensions().unwrap_or(0).to_string(),
      ),
      ("ai.usage.input_tokens", resp.usage.input_tokens.to_string()),
    ];
    notes
      .into_iter()
      .fold(succeed(()), |acc, (key, value)| {
        acc.flat_map(move |()| annotate_current_span(key, value))
      })
      .map(move |()| resp)
  });
  with_ai_request_span(vendor, "embed", &req.model, effect)
}
