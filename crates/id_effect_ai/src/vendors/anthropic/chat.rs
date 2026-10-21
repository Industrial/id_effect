//! Anthropic Messages API (`POST /v1/messages`), buffered and streamed (SSE).
//!
//! System messages map to the top-level `system` field (several are joined by a blank line).
//! Every request carries `max_tokens` (the request's
//! [`max_tokens`](crate::ChatRequest::max_tokens), else
//! [`AiLimits::max_tokens`](crate::AiLimits::max_tokens)). The API key is held as a
//! [`Secret`] and only exposed to build the `x-api-key` header.

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HCons, HNil, Layer, Stream};
use id_effect_config::Secret;
use id_effect_platform::http::{HttpClient, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::chat::{
  ChatMessage, ChatRequest, ChatResponse, ChatRole, LanguageModel, LanguageModelService, Usage,
  provide_language_model,
};
use crate::config::AiConfig;
use crate::error::{AiError, truncate_detail};
use crate::http_util::join_url;
use crate::stream::{CompletionChunk, SseMessage, SseStep};
use crate::tracing_util::{observe_chat, observe_chat_stream};
use crate::vendors::Transport;

/// The `anthropic-version` header sent with every request.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic Messages API client. `Debug` never prints the API key.
#[derive(Clone)]
pub struct AnthropicLanguageModel {
  transport: Transport,
  api_key: Secret<String>,
  base_url: String,
}

impl fmt::Debug for AnthropicLanguageModel {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AnthropicLanguageModel")
      .field("base_url", &self.base_url)
      .field("api_key", &self.api_key)
      .field("transport", &self.transport)
      .finish()
  }
}

impl AnthropicLanguageModel {
  /// Create from the platform HTTP client and config (requires the API key).
  pub fn new(client: Arc<dyn HttpClient>, config: &AiConfig) -> Result<Self, AiError> {
    let api_key = config.require_anthropic_key()?.clone();
    Ok(Self {
      transport: Transport::new(client, config),
      api_key,
      base_url: config.anthropic_base_url.clone(),
    })
  }

  /// Create with the client [`AiConfig::http_client`] builds (SSRF policy and timeouts of
  /// [`AiConfig::http`]).
  pub fn from_config(config: &AiConfig) -> Result<Self, AiError> {
    Self::new(config.http_client(), config)
  }

  fn split_system(messages: &[ChatMessage]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut system = Vec::new();
    let mut out = Vec::new();
    for m in messages {
      match m.role {
        ChatRole::System => system.push(m.content.as_str()),
        ChatRole::User => out.push(json!({ "role": "user", "content": m.content })),
        ChatRole::Assistant => out.push(json!({ "role": "assistant", "content": m.content })),
      }
    }
    ((!system.is_empty()).then(|| system.join("\n\n")), out)
  }

  /// The validated Messages API request for `req` (`stream` selects SSE).
  fn messages_request(&self, req: &ChatRequest, stream: bool) -> Result<HttpRequest, AiError> {
    req.validate()?;
    let (system, messages) = Self::split_system(&req.messages);
    let mut body = json!({
      "model": req.model,
      "max_tokens": req.effective_max_tokens(self.transport.limits.max_tokens),
      "messages": messages,
    });
    if stream {
      body["stream"] = json!(true);
    }
    if let Some(s) = system {
      body["system"] = json!(s);
    }
    self.transport.post_json(
      join_url(&self.base_url, "v1/messages"),
      vec![
        ("x-api-key".to_string(), self.api_key.expose().clone()),
        (
          "anthropic-version".to_string(),
          ANTHROPIC_VERSION.to_string(),
        ),
      ],
      &body,
      stream,
    )
  }

  fn parse_completion(resp: HttpResponse) -> Result<ChatResponse, AiError> {
    let parsed: AnthropicResponse =
      serde_json::from_slice(&resp.body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
    let content: String = parsed
      .content
      .iter()
      .filter(|b| b.kind == "text")
      .filter_map(|b| b.text.as_deref())
      .collect();
    if content.is_empty() {
      return Err(AiError::EmptyResponse);
    }
    let usage = parsed.usage.map_or_else(Usage::default, |u| {
      Usage::new(u.input_tokens.unwrap_or(0), u.output_tokens.unwrap_or(0))
    });
    let resp = ChatResponse::text(content).with_usage(usage);
    Ok(match parsed.stop_reason {
      Some(reason) => resp.with_stop_reason(reason),
      None => resp,
    })
  }
}

#[derive(Deserialize)]
struct AnthropicResponse {
  content: Vec<AnthropicBlock>,
  usage: Option<AnthropicUsage>,
  stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicBlock {
  #[serde(rename = "type")]
  kind: String,
  text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
  input_tokens: Option<u32>,
  output_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct AnthropicStreamEvent {
  #[serde(rename = "type")]
  kind: String,
  delta: Option<AnthropicStreamDelta>,
  error: Option<AnthropicStreamError>,
}

#[derive(Deserialize)]
struct AnthropicStreamDelta {
  text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicStreamError {
  message: Option<String>,
}

/// One Anthropic stream event: `content_block_delta` text is a delta, `message_stop` ends the
/// stream, `error` fails it, every other type (`ping`, `message_start`, ...) is skipped.
fn decode_stream_event(msg: &SseMessage) -> SseStep {
  let data = msg.data();
  if data.trim().is_empty() {
    return SseStep::Skip;
  }
  let ev = match serde_json::from_str::<AnthropicStreamEvent>(&data) {
    Ok(ev) => ev,
    Err(e) => return SseStep::Fail(AiError::InvalidJson(e.to_string())),
  };
  match ev.kind.as_str() {
    "content_block_delta" => match ev.delta.and_then(|d| d.text) {
      Some(text) if !text.is_empty() => SseStep::Delta(text),
      _ => SseStep::Skip,
    },
    "message_stop" => SseStep::Done,
    "error" => SseStep::Fail(AiError::Upstream(truncate_detail(format!(
      "stream error: {}",
      ev.error.and_then(|e| e.message).unwrap_or_default()
    )))),
    _ => SseStep::Skip,
  }
}

impl LanguageModel for AnthropicLanguageModel {
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError> {
    let this = self.clone();
    let body = req.clone();
    let call = self.transport.call(
      Arc::new(move || this.messages_request(&body, false)),
      Self::parse_completion,
    );
    observe_chat("anthropic", &req, self.transport.log_prompts, call)
  }

  fn complete_stream(&self, req: ChatRequest) -> Effect<Stream<CompletionChunk, AiError>, AiError> {
    let this = self.clone();
    let body = req.clone();
    let open = self.transport.open_stream(
      Arc::new(move || this.messages_request(&body, true)),
      decode_stream_event,
    );
    observe_chat_stream("anthropic", &req, self.transport.log_prompts, open)
  }
}

/// Register Anthropic [`LanguageModel`]: a [`Layer`] providing [`LanguageModelService`].
pub fn provide_anthropic_language_model<E: Send + 'static>(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Result<Layer<HCons<LanguageModelService, HNil>, E>, AiError> {
  Ok(provide_language_model(AnthropicLanguageModel::new(
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
    assert_eq!(event(" "), SseStep::Skip);
    assert_eq!(
      event(r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"yo"}}"#),
      SseStep::Delta("yo".into())
    );
    assert_eq!(
      event(r#"{"type":"content_block_delta","delta":{"type":"input_json_delta"}}"#),
      SseStep::Skip
    );
    assert_eq!(event(r#"{"type":"ping"}"#), SseStep::Skip);
    assert_eq!(event(r#"{"type":"message_stop"}"#), SseStep::Done);
    assert!(matches!(
      event(r#"{"type":"error","error":{"message":"Overloaded"}}"#),
      SseStep::Fail(AiError::Upstream(m)) if m.contains("Overloaded")
    ));
    assert!(matches!(event("{"), SseStep::Fail(AiError::InvalidJson(_))));
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
    let resp = AnthropicLanguageModel::parse_completion(response(
      r#"{"content":[{"type":"text","text":"a"},{"type":"tool_use"},{"type":"text","text":"b"}],"usage":{"input_tokens":4,"output_tokens":5},"stop_reason":"end_turn"}"#,
    ))
    .expect("parse");
    assert_eq!(resp.content, "ab");
    assert_eq!(resp.usage, Usage::new(4, 5));
    assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
    let resp = AnthropicLanguageModel::parse_completion(response(
      r#"{"content":[{"type":"text","text":"x"}]}"#,
    ))
    .expect("parse");
    assert_eq!(resp.usage, Usage::default());
    assert_eq!(
      AnthropicLanguageModel::parse_completion(response(r#"{"content":[]}"#)),
      Err(AiError::EmptyResponse)
    );
    assert!(matches!(
      AnthropicLanguageModel::parse_completion(response("[")),
      Err(AiError::InvalidJson(_))
    ));
  }

  #[test]
  fn system_messages_are_joined() {
    let (system, messages) = AnthropicLanguageModel::split_system(&[
      ChatMessage::system("a"),
      ChatMessage::user("u"),
      ChatMessage::system("b"),
      ChatMessage::assistant("x"),
    ]);
    assert_eq!(system.as_deref(), Some("a\n\nb"));
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["role"], "assistant");
    let (system, _) = AnthropicLanguageModel::split_system(&[ChatMessage::user("u")]);
    assert_eq!(system, None);
  }
}
