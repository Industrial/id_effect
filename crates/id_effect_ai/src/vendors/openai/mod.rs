//! OpenAI (`feature = "openai"`): Chat Completions in [`chat`], covering the ChatGPT model ids
//! and OpenAI-compatible servers; embeddings in `embed` (feature `embeddings`).

pub mod chat;
#[cfg(feature = "embeddings")]
pub mod embed;

pub use chat::{OpenAiLanguageModel, provide_openai_language_model};
