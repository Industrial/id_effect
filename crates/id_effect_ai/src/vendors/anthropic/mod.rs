//! Anthropic (`feature = "anthropic"`): the Messages API in [`chat`]. Anthropic serves no
//! embeddings endpoint, so `embed` (feature `embeddings`) holds no adapter.

pub mod chat;
#[cfg(feature = "embeddings")]
pub mod embed;

pub use chat::{AnthropicLanguageModel, provide_anthropic_language_model};
