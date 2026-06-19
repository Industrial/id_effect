//! Language model trait and mock implementation.

use crate::error::AiError;
use crate::streaming::{CompletionChunk, mock_chunk_stream};
use id_effect::kernel::Effect;
use id_effect::{Needs, ProviderBox, provide};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Chat role label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatRole {
  /// System prompt.
  System,
  /// User message.
  User,
  /// Assistant reply.
  Assistant,
}

/// Single chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
  /// Role of the speaker.
  pub role: ChatRole,
  /// Message text.
  pub content: String,
}

/// Request for a chat completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
  /// Model id (vendor-specific string).
  pub model: String,
  /// Conversation messages.
  pub messages: Vec<ChatMessage>,
}

impl ChatRequest {
  /// Validates non-empty messages.
  pub fn validate(&self) -> Result<(), AiError> {
    if self.messages.is_empty() {
      return Err(AiError::EmptyRequest);
    }
    Ok(())
  }
}

/// Buffered completion response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResponse {
  /// Assistant text.
  pub content: String,
  /// Token estimate (mock / vendor metadata).
  pub tokens_used: u32,
}

/// Vendor-neutral language model capability.
#[::id_effect::capability(Arc<dyn LanguageModel>)]
pub trait LanguageModel: Send + Sync + 'static {
  /// Buffered completion.
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError, ()>;
  /// Streaming token chunks.
  fn complete_stream(
    &self,
    req: ChatRequest,
  ) -> Effect<id_effect::Stream<CompletionChunk, AiError, ()>, AiError, ()>;
}

/// Deterministic mock for tests and examples.
#[derive(Debug, Default, Clone)]
pub struct MockLanguageModel {
  reply_prefix: String,
}

impl MockLanguageModel {
  /// Creates a mock that echoes the last user message.
  #[inline]
  pub fn echo() -> Self {
    Self {
      reply_prefix: "echo:".to_string(),
    }
  }

  fn last_user(req: &ChatRequest) -> Result<String, AiError> {
    req.validate()?;
    req
      .messages
      .iter()
      .rev()
      .find(|m| m.role == ChatRole::User)
      .map(|m| m.content.clone())
      .ok_or(AiError::EmptyRequest)
  }
}

impl LanguageModel for MockLanguageModel {
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError, ()> {
    let prefix = self.reply_prefix.clone();
    Effect::new(move |_r: &mut ()| {
      let user = Self::last_user(&req)?;
      let content = format!("{prefix}{user}");
      let tokens_used = content.len() as u32;
      Ok(ChatResponse {
        content,
        tokens_used,
      })
    })
  }

  fn complete_stream(
    &self,
    req: ChatRequest,
  ) -> Effect<id_effect::Stream<CompletionChunk, AiError, ()>, AiError, ()> {
    let prefix = self.reply_prefix.clone();
    Effect::new(move |_r: &mut ()| {
      let user = Self::last_user(&req)?;
      let content = format!("{prefix}{user}");
      Ok(mock_chunk_stream(&content, 4))
    })
  }
}

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(LanguageModelKey)]
struct MockLanguageModelProvider;

impl MockLanguageModelProvider {
  fn new() -> Arc<dyn LanguageModel> {
    Arc::new(MockLanguageModel::echo())
  }
}

/// Register mock language model provider.
#[inline]
pub fn provide_mock_language_model() -> ProviderBox {
  provide!(MockLanguageModelProvider)
}

/// Complete using installed [`LanguageModel`] capability.
#[inline]
pub fn complete<R>(req: ChatRequest) -> Effect<ChatResponse, AiError, R>
where
  R: Needs<LanguageModelKey> + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let model = r.need().clone();
    let inner = model.complete(req);
    Box::pin(async move { inner.run(&mut ()).await })
  })
}
