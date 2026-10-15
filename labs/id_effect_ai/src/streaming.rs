//! Streaming completion chunks as `id_effect::next::Stream`.

use crate::error::AiError;
use crate::model::{ChatRequest, LanguageModelService};
use id_effect::next::{Effect, HList, Selector, Stream, need};
use serde::{Deserialize, Serialize};

/// One streamed token or delta from the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionChunk {
  /// Token text.
  pub delta: String,
  /// True on final chunk.
  pub done: bool,
}

/// Complete with streaming using installed [`LanguageModel`](crate::model::LanguageModel) capability.
#[inline]
pub fn complete_stream<R, I>(
  req: ChatRequest,
) -> Effect<Stream<CompletionChunk, AiError>, AiError, R>
where
  R: Selector<LanguageModelService, I> + HList,
{
  need::<LanguageModelService>().flat_map(move |model| model.complete_stream(req).narrow())
}

/// Build a mock chunk stream by splitting `text` into `chunk_size` character runs.
pub fn mock_chunk_stream(text: &str, chunk_size: usize) -> Stream<CompletionChunk, AiError> {
  let parts: Vec<String> = text
    .chars()
    .collect::<Vec<_>>()
    .chunks(chunk_size.max(1))
    .map(|c| c.iter().collect())
    .collect();
  let n = parts.len();
  let chunks = parts
    .into_iter()
    .enumerate()
    .map(|(i, delta)| CompletionChunk {
      delta,
      done: i + 1 == n,
    })
    .collect();
  Stream::from_vec(chunks)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mock_chunk_stream_emits_parts() {
    let stream = mock_chunk_stream("abcd", 2);
    let chunks = stream
      .run_collect()
      .block_on()
      .into_result()
      .expect("collect");
    let text: String = chunks.iter().map(|c| c.delta.clone()).collect();
    assert_eq!(text, "abcd");
  }

  #[test]
  fn completion_chunk_round_trip_json() {
    let chunk = CompletionChunk {
      delta: "tok".into(),
      done: true,
    };
    let json = serde_json::to_string(&chunk).unwrap();
    let back: CompletionChunk = serde_json::from_str(&json).unwrap();
    assert_eq!(back, chunk);
  }
}
