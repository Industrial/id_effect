//! OpenAI Chat Completions API (`/v1/chat/completions`) — covers ChatGPT model IDs.

use std::sync::Arc;

use id_effect::next::{Effect, HCons, HNil, Layer, Stream, fail, hlist, suspend};
use id_effect_platform::http::{HttpClient, HttpMethod, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::config::AiConfig;
use crate::error::AiError;
use crate::http_util::{bearer_header, check_status, from_result, join_url};
use crate::model::{
  ChatMessage, ChatRequest, ChatResponse, ChatRole, LanguageModel, LanguageModelService,
};
use crate::retry::retry_transient_ai_http;
use crate::sse::SseParser;
use crate::streaming::CompletionChunk;
use crate::tracing_util::with_ai_span;

/// OpenAI-compatible chat client.
#[derive(Clone)]
pub struct OpenAiLanguageModel {
  client: Arc<dyn HttpClient>,
  api_key: String,
  base_url: String,
}

impl OpenAiLanguageModel {
  /// Create from HTTP client and config (requires API key).
  pub fn new(client: Arc<dyn HttpClient>, config: &AiConfig) -> Result<Self, AiError> {
    let key = config.require_openai_key()?.expose().clone();
    Ok(Self {
      client,
      api_key: key,
      base_url: config.openai_base_url.clone(),
    })
  }

  fn role_str(role: ChatRole) -> &'static str {
    match role {
      ChatRole::System => "system",
      ChatRole::User => "user",
      ChatRole::Assistant => "assistant",
    }
  }

  fn build_messages(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    messages
      .iter()
      .map(|m| {
        json!({
          "role": Self::role_str(m.role),
          "content": m.content,
        })
      })
      .collect()
  }

  fn url(&self) -> String {
    join_url(&self.base_url, "v1/chat/completions")
  }

  /// The validated chat-completions request for `req` (`stream` selects SSE).
  fn chat_request(&self, req: &ChatRequest, stream: bool) -> Result<HttpRequest, AiError> {
    req.validate()?;
    let body = json!({
      "model": req.model,
      "messages": Self::build_messages(&req.messages),
      "stream": stream,
    });
    let bytes = serde_json::to_vec(&body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
    Ok(self.auth_request(bytes))
  }

  fn parse_completion(resp: HttpResponse) -> Result<ChatResponse, AiError> {
    check_status(resp.status, || {
      String::from_utf8_lossy(&resp.body).to_string()
    })?;
    let parsed: OpenAiCompletionResponse =
      serde_json::from_slice(&resp.body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
    let content = parsed
      .choices
      .first()
      .and_then(|c| c.message.content.clone())
      .filter(|s| !s.is_empty())
      .ok_or(AiError::EmptyResponse)?;
    let tokens_used = parsed.usage.and_then(|u| u.total_tokens).unwrap_or(0);
    Ok(ChatResponse {
      content,
      tokens_used,
    })
  }

  fn auth_request(&self, body: Vec<u8>) -> HttpRequest {
    HttpRequest {
      method: HttpMethod::Post,
      url: self.url(),
      headers: vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        bearer_header(&self.api_key),
      ],
      body: Some(body),
      timeout: None,
      max_body_bytes: None,
    }
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
}

#[derive(Deserialize)]
struct OpenAiMessage {
  content: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
  total_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct OpenAiStreamChunk {
  choices: Vec<OpenAiStreamChoice>,
}

#[derive(Deserialize)]
struct OpenAiStreamChoice {
  delta: OpenAiDelta,
}

#[derive(Deserialize, Default)]
struct OpenAiDelta {
  content: Option<String>,
}

impl LanguageModel for OpenAiLanguageModel {
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError> {
    let this = self.clone();
    let model = req.model.clone();
    with_ai_span(
      "openai",
      "complete",
      &model,
      retry_transient_ai_http(move || {
        let this = this.clone();
        let req = req.clone();
        suspend(move || {
          let http_req = match this.chat_request(&req, false) {
            Ok(r) => r,
            Err(e) => return fail(e),
          };
          this
            .client
            .execute(http_req)
            .map_err(|e| AiError::Upstream(format!("http: {e}")))
            .flat_map(|resp| from_result(OpenAiLanguageModel::parse_completion(resp)))
        })
      }),
    )
  }

  fn complete_stream(&self, req: ChatRequest) -> Effect<Stream<CompletionChunk, AiError>, AiError> {
    let this = self.clone();
    suspend(move || {
      let http_req = match this.chat_request(&req, true) {
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
            .map(|bytes| openai_sse_to_chunk_stream(&String::from_utf8_lossy(&bytes)))
        })
    })
  }
}

fn openai_sse_to_chunk_stream(text: &str) -> Stream<CompletionChunk, AiError> {
  let mut chunks = Vec::new();
  let mut parser = SseParser::new();
  for msg in parser.feed(text) {
    let data = msg.data();
    if data == "[DONE]" {
      break;
    }
    if let Ok(chunk) = serde_json::from_str::<OpenAiStreamChunk>(&data) {
      for choice in chunk.choices {
        if let Some(delta) = choice.delta.content
          && !delta.is_empty()
        {
          chunks.push(CompletionChunk { delta, done: false });
        }
      }
    }
  }
  chunks.push(CompletionChunk {
    delta: String::new(),
    done: true,
  });
  Stream::from_vec(chunks)
}

/// Register OpenAI [`LanguageModel`]: a [`Layer`] providing [`LanguageModelService`].
pub fn provide_openai_language_model<E: Send + 'static>(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Result<Layer<HCons<LanguageModelService, HNil>, E>, AiError> {
  let model: LanguageModelService = Arc::new(OpenAiLanguageModel::new(client, &config)?);
  Ok(Layer::succeed(hlist![model]))
}
