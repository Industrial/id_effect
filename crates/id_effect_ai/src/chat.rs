//! Provider-neutral chat: messages, requests, responses, token usage, and the [`LanguageModel`]
//! capability every vendor adapter (and every test double) implements.
//!
//! A program asks for the capability ([`LanguageModelService`]) in its environment and calls
//! [`complete`]; the edge provides it with a vendor layer
//! (`provide_openai_language_model`, `provide_anthropic_language_model`), the echo
//! [`provide_mock_language_model`], or a scripted model in tests (`testing::ScriptedModel`).
//!
//! ```
//! use id_effect::{HCons, HNil};
//! use id_effect_ai::{AiError, ChatRequest, LanguageModelService, complete, provide_mock_language_model};
//!
//! let resp = complete::<HCons<LanguageModelService, HNil>, _>(ChatRequest::user("mock", "hi"))
//!   .provide_layer(provide_mock_language_model::<AiError>())
//!   .block_on()
//!   .into_result()
//!   .expect("complete");
//! assert_eq!(resp.content, "echo:hi");
//! ```

use std::sync::Arc;

use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, Stream, hlist, need, suspend};
use serde::{Deserialize, Serialize};

use crate::error::AiError;
use crate::http_util::from_result;
use crate::stream::{CompletionChunk, mock_chunk_stream};

/// Chat role label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatRole {
  /// System prompt.
  System,
  /// User message.
  User,
  /// Assistant reply.
  Assistant,
  /// The result of a tool call, fed back to the model (see [`ChatMessage::tool_result`]).
  Tool,
}

/// Single chat message. Build it with [`ChatMessage::new`] or the role constructors
/// (non-exhaustive, so fields can be added without a break).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChatMessage {
  /// Role of the speaker.
  pub role: ChatRole,
  /// Message text (for a [`ChatRole::Tool`] message: the tool's output or refusal).
  pub content: String,
  /// Tool calls an assistant turn asked for (empty otherwise); vendors need them echoed back
  /// before the matching [`ChatRole::Tool`] results.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub tool_calls: Vec<ToolCall>,
  /// For a [`ChatRole::Tool`] message: the [`ToolCall::id`] it answers.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub tool_call_id: Option<String>,
}

impl ChatMessage {
  /// A message from `role`.
  #[inline]
  pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
    Self {
      role,
      content: content.into(),
      tool_calls: Vec::new(),
      tool_call_id: None,
    }
  }

  /// A system message.
  #[inline]
  pub fn system(content: impl Into<String>) -> Self {
    Self::new(ChatRole::System, content)
  }

  /// A user message.
  #[inline]
  pub fn user(content: impl Into<String>) -> Self {
    Self::new(ChatRole::User, content)
  }

  /// An assistant message.
  #[inline]
  pub fn assistant(content: impl Into<String>) -> Self {
    Self::new(ChatRole::Assistant, content)
  }

  /// An assistant turn that asked for `tool_calls` (with its text, possibly empty).
  pub fn assistant_tool_calls(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
    Self {
      tool_calls,
      ..Self::assistant(content)
    }
  }

  /// The result of tool call `tool_call_id`, fed back to the model.
  pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
    Self {
      tool_call_id: Some(tool_call_id.into()),
      ..Self::new(ChatRole::Tool, content)
    }
  }
}

/// A tool the model may call, as sent to the vendor: its `name`, a `description` for the model,
/// and the JSON Schema of its arguments. The `tools` feature builds these from typed tools
/// (`tools::Tool::definition`); non-exhaustive, build it with [`ToolDefinition::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolDefinition {
  /// Tool name (`[A-Za-z0-9_-]{1,64}`, the rule both vendors apply).
  pub name: String,
  /// What the tool does, for the model.
  pub description: String,
  /// JSON Schema (an `object`) of the arguments.
  pub parameters: serde_json::Value,
}

impl ToolDefinition {
  /// A definition of tool `name`.
  pub fn new(
    name: impl Into<String>,
    description: impl Into<String>,
    parameters: serde_json::Value,
  ) -> Self {
    Self {
      name: name.into(),
      description: description.into(),
      parameters,
    }
  }
}

/// Request for a chat completion. Build it with [`ChatRequest::new`] or [`ChatRequest::user`]
/// (the struct is non-exhaustive, so fields can be added without a break).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChatRequest {
  /// Model id (vendor-specific string).
  pub model: String,
  /// Conversation messages.
  pub messages: Vec<ChatMessage>,
  /// Cap on generated tokens for this request. `None` sends the adapter's finite default
  /// ([`AiLimits::max_tokens`](crate::AiLimits::max_tokens), 1024): no request goes out
  /// without a cap (ADR 0013 R4).
  #[serde(default)]
  pub max_tokens: Option<u32>,
  /// Tools the model may call in this request (empty: none). Sending a definition does not let
  /// anything run: the caller decides which calls execute (`tools::ToolSet`, ADR 0015 D9).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub tools: Vec<ToolDefinition>,
}

impl ChatRequest {
  /// A request for `model` over `messages`.
  pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
    Self {
      model: model.into(),
      messages,
      max_tokens: None,
      tools: Vec::new(),
    }
  }

  /// A one-message request: `text` from the user.
  pub fn user(model: impl Into<String>, text: impl Into<String>) -> Self {
    Self::new(model, vec![ChatMessage::user(text)])
  }

  /// Sets [`max_tokens`](Self::max_tokens).
  #[must_use]
  pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
    self.max_tokens = Some(max_tokens);
    self
  }

  /// Sets [`tools`](Self::tools).
  #[must_use]
  pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
    self.tools = tools;
    self
  }

  /// Validates the request: at least one message, a `max_tokens` (when set) above 0, and a
  /// [`tool_call_id`](ChatMessage::tool_call_id) on every [`ChatRole::Tool`] message.
  pub fn validate(&self) -> Result<(), AiError> {
    if self.messages.is_empty() {
      return Err(AiError::EmptyRequest);
    }
    if self.max_tokens == Some(0) {
      return Err(AiError::InvalidRequest(
        "max_tokens must be at least 1".into(),
      ));
    }
    if self
      .messages
      .iter()
      .any(|m| m.role == ChatRole::Tool && m.tool_call_id.is_none())
    {
      return Err(AiError::InvalidRequest(
        "a tool message needs the tool_call_id it answers".into(),
      ));
    }
    Ok(())
  }

  /// The token cap to send: this request's, else `default`.
  #[inline]
  pub fn effective_max_tokens(&self, default: u32) -> u32 {
    self.max_tokens.unwrap_or(default)
  }
}

/// Token usage reported by the vendor (zero when it reports none).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
  /// Prompt tokens.
  pub input_tokens: u32,
  /// Generated tokens.
  pub output_tokens: u32,
}

impl Usage {
  /// Usage of `input_tokens` prompt and `output_tokens` generated tokens.
  #[inline]
  pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
    Self {
      input_tokens,
      output_tokens,
    }
  }

  /// Prompt plus generated tokens (saturating).
  #[inline]
  pub fn total(&self) -> u32 {
    self.input_tokens.saturating_add(self.output_tokens)
  }
}

/// A tool the model asked to call, in a provider-neutral shape: the tool's `name` and its JSON
/// `arguments`, which the caller decodes before running anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
  /// The vendor's id of this call (echoed back with the tool's result).
  pub id: String,
  /// The tool name the model chose.
  pub name: String,
  /// The arguments as the model wrote them (untrusted input).
  pub arguments: serde_json::Value,
}

impl ToolCall {
  /// A call of tool `name` with `arguments`.
  pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: serde_json::Value) -> Self {
    Self {
      id: id.into(),
      name: name.into(),
      arguments,
    }
  }
}

/// Buffered completion response. Build it with [`ChatResponse::text`] (non-exhaustive).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChatResponse {
  /// Assistant text.
  pub content: String,
  /// Token usage.
  pub usage: Usage,
  /// Tool calls the model asked for (empty for a plain reply).
  #[serde(default)]
  pub tool_calls: Vec<ToolCall>,
  /// Why generation stopped, as the vendor reports it (`stop`, `length`, `end_turn`,
  /// `max_tokens`, ...).
  #[serde(default)]
  pub stop_reason: Option<String>,
}

impl ChatResponse {
  /// A reply with `content`, no usage, no tool calls.
  pub fn text(content: impl Into<String>) -> Self {
    Self {
      content: content.into(),
      usage: Usage::default(),
      tool_calls: Vec::new(),
      stop_reason: None,
    }
  }

  /// Sets [`usage`](Self::usage).
  #[must_use]
  pub fn with_usage(mut self, usage: Usage) -> Self {
    self.usage = usage;
    self
  }

  /// Sets [`tool_calls`](Self::tool_calls).
  #[must_use]
  pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
    self.tool_calls = tool_calls;
    self
  }

  /// Sets [`stop_reason`](Self::stop_reason).
  #[must_use]
  pub fn with_stop_reason(mut self, reason: impl Into<String>) -> Self {
    self.stop_reason = Some(reason.into());
    self
  }
}

/// Injectable language model service.
pub type LanguageModelService = Arc<dyn LanguageModel>;

/// Vendor-neutral language model capability.
pub trait LanguageModel: Send + Sync + 'static {
  /// Buffered completion.
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError>;
  /// Streaming token chunks.
  fn complete_stream(&self, req: ChatRequest) -> Effect<Stream<CompletionChunk, AiError>, AiError>;
}

/// A [`Layer`] providing `model` as the [`LanguageModelService`].
pub fn provide_language_model<E: Send + 'static>(
  model: impl LanguageModel,
) -> Layer<HCons<LanguageModelService, HNil>, E> {
  let model: LanguageModelService = Arc::new(model);
  Layer::succeed(hlist![model])
}

/// Deterministic echo model for examples: replies with the last user message, prefixed
/// `echo:`. Tests that need scripted replies, tool calls or failures use
/// `testing::ScriptedModel` (feature `test-util`).
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
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError> {
    let prefix = self.reply_prefix.clone();
    suspend(move || {
      from_result(Self::last_user(&req).map(|user| {
        let content = format!("{prefix}{user}");
        let output_tokens = u32::try_from(content.len()).unwrap_or(u32::MAX);
        ChatResponse::text(content).with_usage(Usage::new(0, output_tokens))
      }))
    })
  }

  fn complete_stream(&self, req: ChatRequest) -> Effect<Stream<CompletionChunk, AiError>, AiError> {
    let prefix = self.reply_prefix.clone();
    suspend(move || {
      from_result(
        Self::last_user(&req).map(|user| mock_chunk_stream(&format!("{prefix}{user}"), 4)),
      )
    })
  }
}

/// Register mock language model provider: a [`Layer`] providing [`LanguageModelService`]
/// ([`MockLanguageModel::echo`]).
#[inline]
pub fn provide_mock_language_model<E: Send + 'static>()
-> Layer<HCons<LanguageModelService, HNil>, E> {
  provide_language_model(MockLanguageModel::echo())
}

/// Complete using installed [`LanguageModel`] capability.
#[inline]
pub fn complete<R, I>(req: ChatRequest) -> Effect<ChatResponse, AiError, R>
where
  R: Selector<LanguageModelService, I> + HList,
{
  need::<LanguageModelService>().flat_map(move |model| model.complete(req).narrow())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn message_constructors_set_roles() {
    assert_eq!(ChatMessage::system("s").role, ChatRole::System);
    assert_eq!(ChatMessage::user("u").role, ChatRole::User);
    assert_eq!(ChatMessage::assistant("a").role, ChatRole::Assistant);
    let call = ToolCall::new("c1", "lookup", serde_json::json!({}));
    let turn = ChatMessage::assistant_tool_calls("", vec![call.clone()]);
    assert_eq!(turn.role, ChatRole::Assistant);
    assert_eq!(turn.tool_calls, vec![call]);
    let result = ChatMessage::tool_result("c1", "{}");
    assert_eq!(result.role, ChatRole::Tool);
    assert_eq!(result.tool_call_id.as_deref(), Some("c1"));
  }

  #[test]
  fn tool_messages_and_definitions_validate_and_serialize() {
    let def = ToolDefinition::new("lookup", "find", serde_json::json!({"type": "object"}));
    let req = ChatRequest::new("m", vec![ChatMessage::tool_result("c1", "ok")])
      .with_tools(vec![def.clone()]);
    assert_eq!(req.validate(), Ok(()));
    assert_eq!(req.tools, vec![def]);
    let orphan = ChatMessage::new(ChatRole::Tool, "ok");
    assert!(matches!(
      ChatRequest::new("m", vec![orphan]).validate(),
      Err(AiError::InvalidRequest(_))
    ));
    // Plain messages serialize without the tool fields.
    let wire = serde_json::to_value(ChatMessage::user("x")).expect("encode");
    assert_eq!(wire, serde_json::json!({"role": "User", "content": "x"}));
  }

  #[test]
  fn request_validation_and_max_tokens() {
    let req = ChatRequest::user("m", "hi");
    assert_eq!(req.validate(), Ok(()));
    assert_eq!(req.effective_max_tokens(9), 9);
    let req = req.with_max_tokens(3);
    assert_eq!(req.effective_max_tokens(9), 3);
    assert!(matches!(
      req.with_max_tokens(0).validate(),
      Err(AiError::InvalidRequest(_))
    ));
    assert_eq!(
      ChatRequest::new("m", vec![]).validate(),
      Err(AiError::EmptyRequest)
    );
  }

  #[test]
  fn request_deserializes_without_max_tokens() {
    let req: ChatRequest =
      serde_json::from_str(r#"{"model":"m","messages":[{"role":"User","content":"x"}]}"#)
        .expect("decode");
    assert_eq!(req.max_tokens, None);
  }

  #[test]
  fn response_builders_and_usage() {
    let call = ToolCall::new("c1", "lookup", serde_json::json!({"q": 1}));
    let resp = ChatResponse::text("hi")
      .with_usage(Usage::new(2, 3))
      .with_tool_calls(vec![call.clone()])
      .with_stop_reason("stop");
    assert_eq!(resp.usage.total(), 5);
    assert_eq!(resp.tool_calls, vec![call]);
    assert_eq!(resp.stop_reason.as_deref(), Some("stop"));
    assert_eq!(Usage::new(u32::MAX, 1).total(), u32::MAX);
  }

  #[test]
  fn mock_without_user_message_is_empty_request() {
    let req = ChatRequest::new("m", vec![ChatMessage::system("s")]);
    let err = MockLanguageModel::echo()
      .complete(req.clone())
      .block_on()
      .into_result()
      .unwrap_err();
    assert_eq!(err.fail_option(), Some(AiError::EmptyRequest));
    assert!(
      MockLanguageModel::echo()
        .complete_stream(req)
        .block_on()
        .into_result()
        .is_err()
    );
  }
}
