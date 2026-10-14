//! HTTP vendor adapters implementing [`LanguageModel`](crate::model::LanguageModel).

#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "anthropic")]
pub use anthropic::{AnthropicLanguageModel, provide_anthropic_language_model};
#[cfg(feature = "openai")]
pub use openai::{OpenAiLanguageModel, provide_openai_language_model};
