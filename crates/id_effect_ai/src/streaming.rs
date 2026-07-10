//! Streaming completion chunks as `id_effect::Stream`.

use crate::error::AiError;
use crate::model::{ChatRequest, LanguageModelService};
use id_effect::kernel::Effect;
use id_effect::{Chunk, Needs, end_stream, send_chunk, stream_from_channel};
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
pub fn complete_stream<R>(
  req: ChatRequest,
) -> Effect<id_effect::Stream<CompletionChunk, AiError, ()>, AiError, R>
where
  R: Needs<LanguageModelService> + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let model = r.need().clone();
    let inner = model.complete_stream(req);
    Box::pin(async move { inner.run(&mut ()).await })
  })
}

/// Build a mock chunk stream by splitting `text` into `chunk_size` character runs.
pub fn mock_chunk_stream(
  text: &str,
  chunk_size: usize,
) -> id_effect::Stream<CompletionChunk, AiError, ()> {
  let (stream, sender) = stream_from_channel::<CompletionChunk, AiError, ()>(8);
  let parts: Vec<String> = text
    .chars()
    .collect::<Vec<_>>()
    .chunks(chunk_size.max(1))
    .map(|c| c.iter().collect())
    .collect();
  let n = parts.len();
  std::thread::spawn(move || {
    for (i, part) in parts.into_iter().enumerate() {
      let chunk = CompletionChunk {
        delta: part,
        done: i + 1 == n,
      };
      if id_effect::run_blocking(send_chunk(&sender, Chunk::singleton(chunk)), ()).is_err() {
        return;
      }
    }
    let _ = id_effect::run_blocking(end_stream(sender), ());
  });
  stream
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::runtime::run_blocking;

  #[test]
  fn mock_chunk_stream_emits_parts() {
    let stream = mock_chunk_stream("abcd", 2);
    let chunks = run_blocking(stream.run_collect(), ()).expect("collect");
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
