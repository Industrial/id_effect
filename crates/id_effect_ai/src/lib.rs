//! Provider-neutral LLM chat and streaming for `id_effect`, secure by default.
//!
//! - **Chat** ([`chat`]): [`LanguageModel`] is a capability in the typed environment;
//!   [`complete`] reads it and returns an `Effect<ChatResponse, AiError, R>`. Vendor layers:
//!   OpenAI (`openai`) and Anthropic (`anthropic`); the Cursor Cloud Agents client sits behind
//!   the non-default `cursor` feature.
//! - **Streaming** ([`stream`]): [`complete_stream`] yields an `id_effect::Stream` of
//!   [`CompletionChunk`]s, decoded incrementally from the vendor's server-sent events.
//! - **Retries, spans, usage**: transient statuses are retried with backoff
//!   ([`retry_transient_ai_http`]); every call runs in an `ai.request` span with vendor, model
//!   and token usage ([`with_ai_request_span`]).
//! - **Testing** (`testing`, feature `test-util`): `ScriptedModel` replays deterministic
//!   replies, tool calls, streams and failures, so CI never calls a vendor (ADR 0015 D8).
//!
//! Secure by default (ADR 0013): every HTTP call goes through id_effect_platform's client
//! (SSRF policy, no redirects, timeouts, body caps); API keys are `id_effect_config::Secret`;
//! prompts and completions are never logged or put on spans unless
//! [`AiConfig::dangerous_log_prompts`] is taken (recorded in the posture registry); `max_tokens`
//! has a finite default and a stream has byte and event caps ([`AiLimits`]).
//!
//! ```
//! use id_effect::{HCons, HNil};
//! use id_effect_ai::{AiError, ChatRequest, LanguageModelService, complete, provide_mock_language_model};
//!
//! let reply = complete::<HCons<LanguageModelService, HNil>, _>(ChatRequest::user("mock", "hello"))
//!   .provide_layer(provide_mock_language_model::<AiError>())
//!   .block_on()
//!   .into_result()
//!   .expect("complete");
//! assert_eq!(reply.content, "echo:hello");
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(clippy::new_ret_no_self)]

pub mod chat;
mod config;
mod error;
mod http_util;
mod retry;
pub mod stream;
mod tracing_util;

#[cfg(feature = "test-util")]
pub mod testing;

#[cfg(any(feature = "openai", feature = "anthropic", feature = "ollama"))]
pub mod vendors;

#[cfg(feature = "cursor")]
pub mod cursor;

#[cfg(feature = "embeddings")]
pub mod embeddings;
#[cfg(feature = "mcp")]
pub mod mcp;
#[cfg(feature = "tools")]
pub mod agent;
#[cfg(feature = "tools")]
pub mod structured;
#[cfg(feature = "tools")]
pub mod tools;

pub use chat::{
  ChatMessage, ChatRequest, ChatResponse, ChatRole, LanguageModel, LanguageModelService,
  MockLanguageModel, ToolCall, ToolDefinition, Usage, complete, provide_language_model,
  provide_mock_language_model,
};
pub use config::{AiConfig, AiLimits, DANGEROUS_LOG_PROMPTS};
pub use error::AiError;
pub use http_util::{bearer_header, cursor_basic_auth_header, join_url};
pub use retry::{default_ai_retry_schedule, retry_transient_ai_http};
pub use stream::{
  CompletionChunk, SseField, SseMessage, SseParser, complete_stream, decode_sse_event,
  mock_chunk_stream,
};
pub use tracing_util::{AI_SPAN, with_ai_request_span, with_ai_span};

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

#[cfg(feature = "embeddings")]
pub use embeddings::{
  EmbedRequest, EmbedResponse, Embedder, EmbedderService, Embedding, EmbeddingError,
  EmbeddingLimits, cosine_similarity, dot_product, embed, provide_embedder, top_k_similar,
};
#[cfg(all(feature = "embeddings", feature = "ollama"))]
pub use vendors::ollama::embed::{OllamaEmbedder, provide_ollama_embedder};
#[cfg(all(feature = "embeddings", feature = "openai"))]
pub use vendors::openai::embed::{OpenAiEmbedder, provide_openai_embedder};
