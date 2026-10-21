//! OpenAI Chat Completions API (`POST /v1/chat/completions`), buffered and streamed (SSE).
//!
//! Every request carries `max_completion_tokens` (the request's
//! [`max_tokens`](crate::ChatRequest::max_tokens), else
//! [`AiLimits::max_tokens`](crate::AiLimits::max_tokens)). The API key is held as a
//! [`Secret`] and only exposed to build the `Authorization` header.
//!
//! Tool calling: [`ChatRequest::tools`] are sent as `tools` of `type: "function"`; an assistant
//! turn with tool calls is sent with its `tool_calls` (arguments as JSON text), and a
//! [`ChatRole::Tool`] message as `role: "tool"` with its `tool_call_id`. A reply's
//! `message.tool_calls` become [`ToolCall`]s; arguments that are not valid JSON are kept as a
//! JSON string, which no object schema decodes, so the call is refused rather than guessed.

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HCons, HNil, Layer, Stream};
use id_effect_config::Secret;
use id_effect_platform::http::{HttpClient, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::chat::{
  ChatMessage, ChatRequest, ChatResponse, ChatRole, LanguageModel, LanguageModelService, ToolCall,
  ToolDefinition, Usage, provide_language_model,
};
use crate::config::AiConfig;
use crate::error::{AiError, truncate_detail};
use crate::http_util::{bearer_header, join_url};
use crate::stream::{CompletionChunk, SseMessage, SseStep};
use crate::tracing_util::{observe_chat, observe_chat_stream};
use crate::vendors::Transport;

/// OpenAI-compatible chat client. `Debug` never prints the API key.
#[derive(Clone)]
pub struct OpenAiLanguageModel {
  transport: Transport,
  api_key: Secret<String>,
  base_url: String,
}

impl fmt::Debug for OpenAiLanguageModel {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OpenAiLanguageModel")
      .field("base_url", &self.base_url)
      .field("api_key", &self.api_key)
      .field("transport", &self.transport)
      .finish()
  }
}

impl OpenAiLanguageModel {
  /// Create from the platform HTTP client and config (requires the API key).
  pub fn new(client: Arc<dyn HttpClient>, config: &AiConfig) -> Result<Self, AiError> {
    let api_key = config.require_openai_key()?.clone();
    Ok(Self {
      transport: Transport::new(client, config),
      api_key,
      base_url: config.openai_base_url.clone(),
    })
  }

  /// Create with the client [`AiConfig::http_client`] builds (SSRF policy and timeouts of
  /// [`AiConfig::http`]).
  pub fn from_config(config: &AiConfig) -> Result<Self, AiError> {
    Self::new(config.http_client(), config)
  }

  fn role_str(role: ChatRole) -> &'static str {
    match role {
      ChatRole::System => "system",
      ChatRole::User => "user",
      ChatRole::Assistant => "assistant",
      ChatRole::Tool => "tool",
    }
  }

  /// One wire message: tool results carry `tool_call_id`, assistant tool-call turns carry
  /// `tool_calls` (and `content: null` when they have no text).
  fn build_message(m: &ChatMessage) -> serde_json::Value {
    let mut msg = json!({ "role": Self::role_str(m.role), "content": m.content });
    if let Some(id) = &m.tool_call_id {
      msg["tool_call_id"] = json!(id);
    }
    if !m.tool_calls.is_empty() {
      if m.content.is_empty() {
        msg["content"] = serde_json::Value::Null;
      }
      msg["tool_calls"] = m
        .tool_calls
        .iter()
        .map(|c| {
          json!({
            "id": c.id,
            "type": "function",
            "function": { "name": c.name, "arguments": arguments_text(&c.arguments) },
          })
        })
        .collect();
    }
    msg
  }

  fn build_messages(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    messages.iter().map(Self::build_message).collect()
  }

  fn build_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
      .iter()
      .map(|t| {
        json!({
          "type": "function",
          "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
        })
      })
      .collect()
  }

  /// The validated chat-completions request for `req` (`stream` selects SSE).
  fn chat_request(&self, req: &ChatRequest, stream: bool) -> Result<HttpRequest, AiError> {
    req.validate()?;
    let mut body = json!({
      "model": req.model,
      "messages": Self::build_messages(&req.messages),
      "max_completion_tokens": req.effective_max_tokens(self.transport.limits.max_tokens),
      "stream": stream,
    });
    if !req.tools.is_empty() {
      body["tools"] = json!(Self::build_tools(&req.tools));
    }
    self.transport.post_json(
      join_url(&self.base_url, "v1/chat/completions"),
      vec![bearer_header(self.api_key.expose())],
      &body,
      stream,
    )
  }

  fn parse_completion(resp: HttpResponse) -> Result<ChatResponse, AiError> {
    let parsed: OpenAiCompletionResponse =
      serde_json::from_slice(&resp.body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
    let choice = parsed
      .choices
      .into_iter()
      .next()
      .ok_or(AiError::EmptyResponse)?;
    let stop_reason = choice.finish_reason;
    let content = choice.message.content.unwrap_or_default();
    let tool_calls: Vec<ToolCall> = choice
      .message
      .tool_calls
      .unwrap_or_default()
      .into_iter()
      .map(|c| ToolCall::new(c.id, c.function.name, parse_arguments(c.function.arguments)))
      .collect();
    if content.is_empty() && tool_calls.is_empty() {
      return Err(AiError::EmptyResponse);
    }
    let usage = parsed.usage.map_or_else(Usage::default, |u| {
      Usage::new(
        u.prompt_tokens.unwrap_or(0),
        u.completion_tokens.unwrap_or(0),
      )
    });
    let resp = ChatResponse::text(content)
      .with_usage(usage)
      .with_tool_calls(tool_calls);
    Ok(match stop_reason {
      Some(reason) => resp.with_stop_reason(reason),
      None => resp,
    })
  }
}

#[derive(Deserialize)]
struct OpenAiCompletionResponse {
  choices: Vec<OpenAiChoice>,
  usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
  message: OpenAiMessage,
  finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiMessage {
  content: Option<String>,
  tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Deserialize)]
struct OpenAiToolCall {
  id: String,
  function: OpenAiFunctionCall,
}

#[derive(Deserialize)]
struct OpenAiFunctionCall {
  name: String,
  #[serde(default)]
  arguments: String,
}

/// A call's `arguments` text as JSON; text that is not JSON stays a JSON string (refused by
/// every object schema). Empty text is an empty object.
fn parse_arguments(text: String) -> serde_json::Value {
  if text.trim().is_empty() {
    return json!({});
  }
  serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
}

/// The wire `arguments` text of a call: a string as is (already text), anything else as JSON.
fn arguments_text(arguments: &serde_json::Value) -> String {
  match arguments {
    serde_json::Value::String(s) => s.clone(),
    other => other.to_string(),
  }
}

#[derive(Deserialize)]
struct OpenAiUsage {
  prompt_tokens: Option<u32>,
  completion_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct OpenAiStreamChunk {
  #[serde(default)]
  choices: Vec<OpenAiStreamChoice>,
  error: Option<OpenAiStreamError>,
}

#[derive(Deserialize)]
struct OpenAiStreamChoice {
  #[serde(default)]
  delta: OpenAiDelta,
}

#[derive(Deserialize, Default)]
struct OpenAiDelta {
  content: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiStreamError {
  message: Option<String>,
}

/// One OpenAI stream event: `[DONE]` ends the stream, an `error` object fails it, text deltas
/// of every choice are concatenated.
fn decode_stream_event(msg: &SseMessage) -> SseStep {
  let data = msg.data();
  let data = data.trim();
  if data == "[DONE]" {
    return SseStep::Done;
  }
  if data.is_empty() {
    return SseStep::Skip;
  }
  match serde_json::from_str::<OpenAiStreamChunk>(data) {
    Ok(OpenAiStreamChunk {
      error: Some(error), ..
    }) => SseStep::Fail(AiError::Upstream(truncate_detail(format!(
      "stream error: {}",
      error.message.unwrap_or_default()
    )))),
    Ok(chunk) => {
      let delta: String = chunk
        .choices
        .into_iter()
        .filter_map(|c| c.delta.content)
        .collect();
      if delta.is_empty() {
        SseStep::Skip
      } else {
        SseStep::Delta(delta)
      }
    }
    Err(e) => SseStep::Fail(AiError::InvalidJson(e.to_string())),
  }
}

impl LanguageModel for OpenAiLanguageModel {
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError> {
    let this = self.clone();
    let body = req.clone();
    let call = self.transport.call(
      Arc::new(move || this.chat_request(&body, false)),
      Self::parse_completion,
    );
    observe_chat("openai", &req, self.transport.log_prompts, call)
  }

  fn complete_stream(&self, req: ChatRequest) -> Effect<Stream<CompletionChunk, AiError>, AiError> {
    let this = self.clone();
    let body = req.clone();
    let open = self.transport.open_stream(
      Arc::new(move || this.chat_request(&body, true)),
      decode_stream_event,
    );
    observe_chat_stream("openai", &req, self.transport.log_prompts, open)
  }
}

/// Register OpenAI [`LanguageModel`]: a [`Layer`] providing [`LanguageModelService`].
pub fn provide_openai_language_model<E: Send + 'static>(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Result<Layer<HCons<LanguageModelService, HNil>, E>, AiError> {
  Ok(provide_language_model(OpenAiLanguageModel::new(
    client, &config,
  )?))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn event(data: &str) -> SseStep {
    decode_stream_event(&SseMessage {
      event: None,
      data_lines: vec![data.to_string()],
    })
  }

  #[test]
  fn stream_events_decode() {
    assert_eq!(event("[DONE]"), SseStep::Done);
    assert_eq!(event(""), SseStep::Skip);
    assert_eq!(
      event(r#"{"choices":[{"delta":{"content":"a"}},{"delta":{"content":"b"}}]}"#),
      SseStep::Delta("ab".into())
    );
    assert_eq!(event(r#"{"choices":[{"delta":{}}]}"#), SseStep::Skip);
    assert_eq!(event(r#"{"choices":[{}]}"#), SseStep::Skip);
    assert!(matches!(
      event(r#"{"error":{"message":"overloaded"}}"#),
      SseStep::Fail(AiError::Upstream(m)) if m.contains("overloaded")
    ));
    assert!(matches!(
      event("not json"),
      SseStep::Fail(AiError::InvalidJson(_))
    ));
  }

  fn response(body: &str) -> HttpResponse {
    HttpResponse {
      status: 200,
      headers: vec![],
      body: body.as_bytes().to_vec(),
    }
  }

  #[test]
  fn completion_parsing() {
    let resp = OpenAiLanguageModel::parse_completion(response(
      r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"length"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}"#,
    ))
    .expect("parse");
    assert_eq!(resp.content, "hi");
    assert_eq!(resp.usage, Usage::new(2, 3));
    assert_eq!(resp.stop_reason.as_deref(), Some("length"));
    let resp = OpenAiLanguageModel::parse_completion(response(
      r#"{"choices":[{"message":{"content":"x"}}]}"#,
    ))
    .expect("parse");
    assert_eq!(resp.usage, Usage::default());
    assert_eq!(resp.stop_reason, None);
    assert_eq!(
      OpenAiLanguageModel::parse_completion(response(r#"{"choices":[]}"#)),
      Err(AiError::EmptyResponse)
    );
    assert!(matches!(
      OpenAiLanguageModel::parse_completion(response("{")),
      Err(AiError::InvalidJson(_))
    ));
    assert_eq!(
      OpenAiLanguageModel::parse_completion(response(
        r#"{"choices":[{"message":{"content":"","tool_calls":null}}]}"#
      )),
      Err(AiError::EmptyResponse)
    );
  }

  #[test]
  fn tool_calls_parse_from_the_reply() {
    let resp = OpenAiLanguageModel::parse_completion(response(
      r#"{"choices":[{"message":{"content":null,"tool_calls":[
        {"id":"c1","type":"function","function":{"name":"add","arguments":"{\"a\":1}"}},
        {"id":"c2","type":"function","function":{"name":"bad","arguments":"{oops"}},
        {"id":"c3","type":"function","function":{"name":"none"}}
      ]},"finish_reason":"tool_calls"}]}"#,
    ))
    .expect("parse");
    assert_eq!(resp.content, "");
    assert_eq!(resp.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(
      resp.tool_calls,
      vec![
        ToolCall::new("c1", "add", json!({"a": 1})),
        ToolCall::new("c2", "bad", json!("{oops")),
        ToolCall::new("c3", "none", json!({})),
      ]
    );
  }

  #[test]
  fn tool_messages_and_definitions_are_written() {
    let calls = vec![
      ToolCall::new("c1", "add", json!({"a": 1})),
      ToolCall::new("c2", "raw", json!("{oops")),
    ];
    let msgs = OpenAiLanguageModel::build_messages(&[
      ChatMessage::assistant_tool_calls("", calls),
      ChatMessage::tool_result("c1", "{\"sum\":1}"),
      ChatMessage::assistant_tool_calls("thinking", vec![ToolCall::new("c3", "x", json!({}))]),
    ]);
    assert_eq!(msgs[0]["content"], serde_json::Value::Null);
    assert_eq!(msgs[0]["tool_calls"][0]["type"], "function");
    assert_eq!(
      msgs[0]["tool_calls"][0]["function"]["arguments"],
      "{\"a\":1}"
    );
    assert_eq!(msgs[0]["tool_calls"][1]["function"]["arguments"], "{oops");
    assert_eq!(
      msgs[1],
      json!({"role": "tool", "content": "{\"sum\":1}", "tool_call_id": "c1"})
    );
    assert_eq!(msgs[2]["content"], "thinking");
    let tools = OpenAiLanguageModel::build_tools(&[ToolDefinition::new(
      "add",
      "adds",
      json!({"type": "object"}),
    )]);
    assert_eq!(
      tools,
      vec![json!({
        "type": "function",
        "function": {"name": "add", "description": "adds", "parameters": {"type": "object"}},
      })]
    );
  }
}
