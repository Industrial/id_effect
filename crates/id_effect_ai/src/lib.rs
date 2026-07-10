//! LLM client traits, streaming completions, and multi-vendor HTTP adapters for `id_effect`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(clippy::new_ret_no_self, clippy::unused_unit)]

mod config;
mod error;
mod http_util;
mod model;
mod retry;
mod sse;
mod streaming;
mod tracing_util;

#[cfg(any(feature = "openai", feature = "anthropic"))]
pub mod vendors;

#[cfg(feature = "cursor")]
pub mod cursor;

pub use config::AiConfig;
pub use error::AiError;
pub use http_util::{bearer_header, cursor_basic_auth_header, join_url};
pub use model::{
  ChatMessage, ChatRequest, ChatResponse, ChatRole, LanguageModel, LanguageModelService,
  MockLanguageModel, complete, provide_mock_language_model,
};
pub use retry::{default_ai_retry_schedule, retry_transient_ai_http};
pub use sse::{SseField, SseMessage, SseParser};
pub use streaming::{CompletionChunk, complete_stream, mock_chunk_stream};
pub use tracing_util::{with_ai_request_span, with_ai_span};

#[cfg(feature = "anthropic")]
pub use vendors::{AnthropicLanguageModel, provide_anthropic_language_model};
#[cfg(feature = "openai")]
pub use vendors::{OpenAiLanguageModel, provide_openai_language_model};

#[cfg(feature = "cursor")]
pub use cursor::{
  CreateAgentRequest, CursorAgent, CursorAgentStatus, CursorAgentsClient,
  CursorAgentsClientService, CursorAgentsError, CursorModel, CursorRepo, CursorRun,
  CursorRunStatus, HttpCursorAgentsClient, provide_cursor_agents_client,
};
