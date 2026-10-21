//! Anthropic Messages API (`/v1/messages`).

use std::sync::Arc;

use id_effect::{Effect, HCons, HNil, Layer, Stream, fail, hlist, suspend};
use id_effect_platform::http::{HttpClient, HttpMethod, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::config::AiConfig;
use crate::error::AiError;
use crate::http_util::{check_status, from_result, join_url};
use crate::model::{
  ChatMessage, ChatRequest, ChatResponse, ChatRole, LanguageModel, LanguageModelService,
};
use crate::retry::retry_transient_ai_http;
use crate::sse::SseParser;
use crate::streaming::CompletionChunk;
use crate::tracing_util::with_ai_span;

/// Anthropic Messages API client.
#[derive(Clone)]
pub struct AnthropicLanguageModel {
  client: Arc<dyn HttpClient>,
  api_key: String,
  base_url: String,
  max_tokens: u32,
}

impl AnthropicLanguageModel {
  /// Create from HTTP client and config.
  pub fn new(client: Arc<dyn HttpClient>, config: &AiConfig) -> Result<Self, AiError> {
    let key = config.require_anthropic_key()?.expose().clone();
    Ok(Self {
      client,
      api_key: key,
      base_url: config.anthropic_base_url.clone(),
      max_tokens: config.anthropic_max_tokens,
    })
  }

  fn role_str(role: ChatRole) -> &'static str {
    match role {
      ChatRole::System => "user",
      ChatRole::User => "user",
      ChatRole::Assistant => "assistant",
    }
  }

  fn split_system(messages: &[ChatMessage]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut system = None;
    let mut out = Vec::new();
    for m in messages {
      if m.role == ChatRole::System {
        system = Some(m.content.clone());
      } else {
        out.push(json!({
          "role": Self::role_str(m.role),
          "content": m.content,
        }));
      }
    }
    (system, out)
  }

  fn url(&self) -> String {
    join_url(&self.base_url, "v1/messages")
  }

  fn auth_request(&self, body: Vec<u8>, stream: bool) -> HttpRequest {
    let mut headers = vec![
      ("Content-Type".to_string(), "application/json".to_string()),
      ("x-api-key".to_string(), self.api_key.clone()),
      ("anthropic-version".to_string(), "2023-06-01".to_string()),
    ];
    if stream {
      headers.push(("Accept".to_string(), "text/event-stream".to_string()));
    }
    HttpRequest {
      method: HttpMethod::Post,
      url: self.url(),
      headers,
      body: Some(body),
      timeout: None,
      max_body_bytes: None,
    }
  }
}

#[derive(Deserialize)]
struct AnthropicResponse {
  content: Vec<AnthropicBlock>,
  usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicBlock {
  #[serde(rename = "type")]
  kind: String,
  text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
  output_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct AnthropicStreamEvent {
  #[serde(rename = "type")]
  kind: String,
  delta: Option<AnthropicStreamDelta>,
}

#[derive(Deserialize)]
struct AnthropicStreamDelta {
  #[serde(rename = "type")]
  _kind: Option<String>,
  text: Option<String>,
}

impl AnthropicLanguageModel {
  /// The validated Messages API request for `req` (`stream` selects SSE).
  fn messages_request(&self, req: &ChatRequest, stream: bool) -> Result<HttpRequest, AiError> {
    req.validate()?;
    let (system, msgs) = Self::split_system(&req.messages);
    let mut body = if stream {
      json!({
        "model": req.model,
        "max_tokens": self.max_tokens,
        "stream": true,
        "messages": msgs,
      })
    } else {
      json!({
        "model": req.model,
        "max_tokens": self.max_tokens,
        "messages": msgs,
      })
    };
    if let Some(s) = system {
      body["system"] = json!(s);
    }
    let bytes = serde_json::to_vec(&body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
    Ok(self.auth_request(bytes, stream))
  }

  fn parse_completion(resp: HttpResponse) -> Result<ChatResponse, AiError> {
    check_status(resp.status, || {
      String::from_utf8_lossy(&resp.body).to_string()
    })?;
    let parsed: AnthropicResponse =
      serde_json::from_slice(&resp.body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
    let content = parsed
      .content
      .iter()
      .find(|b| b.kind == "text")
      .and_then(|b| b.text.clone())
      .filter(|s| !s.is_empty())
      .ok_or(AiError::EmptyResponse)?;
    let tokens_used = parsed.usage.and_then(|u| u.output_tokens).unwrap_or(0);
    Ok(ChatResponse {
      content,
      tokens_used,
    })
  }
}

impl LanguageModel for AnthropicLanguageModel {
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError> {
    let this = self.clone();
    let model = req.model.clone();
    with_ai_span(
      "anthropic",
      "complete",
      &model,
      retry_transient_ai_http(move || {
        let this = this.clone();
        let req = req.clone();
        suspend(move || {
          let http_req = match this.messages_request(&req, false) {
            Ok(r) => r,
            Err(e) => return fail(e),
          };
          this
            .client
            .execute(http_req)
            .map_err(|e| AiError::Upstream(format!("http: {e}")))
            .flat_map(|resp| from_result(AnthropicLanguageModel::parse_completion(resp)))
        })
      }),
    )
  }

  fn complete_stream(&self, req: ChatRequest) -> Effect<Stream<CompletionChunk, AiError>, AiError> {
    let this = self.clone();
    suspend(move || {
      let http_req = match this.messages_request(&req, true) {
        Ok(r) => r,
        Err(e) => return fail(e),
      };
      this
        .client
        .execute_stream(http_req)
        .map_err(|e| AiError::Upstream(format!("http: {e}")))
        .flat_map(|resp| {
          if let Err(e) = check_status(resp.status, || "stream failed".to_string()) {
            return fail(e);
          }
          resp
            .body
            .collect()
            .map_err(|e| AiError::Upstream(format!("stream collect: {e}")))
            .map(|bytes| anthropic_sse_to_chunk_stream(&String::from_utf8_lossy(&bytes)))
        })
    })
  }
}

fn anthropic_sse_to_chunk_stream(text: &str) -> Stream<CompletionChunk, AiError> {
  let mut chunks = Vec::new();
  let mut parser = SseParser::new();
  for msg in parser.feed(text) {
    let data = msg.data();
    if let Ok(ev) = serde_json::from_str::<AnthropicStreamEvent>(&data)
      && ev.kind == "content_block_delta"
      && let Some(delta) = ev.delta.and_then(|d| d.text)
      && !delta.is_empty()
    {
      chunks.push(CompletionChunk { delta, done: false });
    }
  }
  chunks.push(CompletionChunk {
    delta: String::new(),
    done: true,
  });
  Stream::from_vec(chunks)
}

/// Register Anthropic [`LanguageModel`]: a [`Layer`] providing [`LanguageModelService`].
pub fn provide_anthropic_language_model<E: Send + 'static>(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Result<Layer<HCons<LanguageModelService, HNil>, E>, AiError> {
  let model: LanguageModelService = Arc::new(AnthropicLanguageModel::new(client, &config)?);
  Ok(Layer::succeed(hlist![model]))
}
