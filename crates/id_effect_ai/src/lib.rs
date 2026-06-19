//! LLM client traits and streaming completions for `id_effect` (Phase H spike).

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(clippy::new_ret_no_self, clippy::unused_unit)]

mod error;
mod model;
mod streaming;

pub use error::AiError;
pub use model::{
  ChatMessage, ChatRequest, ChatResponse, ChatRole, LanguageModel, LanguageModelKey,
  MockLanguageModel, complete, provide_mock_language_model,
};
pub use streaming::{CompletionChunk, complete_stream, mock_chunk_stream};
