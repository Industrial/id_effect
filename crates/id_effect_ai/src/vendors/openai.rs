//! OpenAI Chat Completions API (`/v1/chat/completions`) — covers ChatGPT model IDs.

use std::sync::Arc;

use id_effect::kernel::Effect;
use id_effect::{Cap, CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};
use id_effect_platform::http::{HttpClient, HttpMethod, HttpRequest};
use serde::Deserialize;
use serde_json::json;

use crate::config::AiConfig;
use crate::error::AiError;
use crate::http_util::{bearer_header, join_url};
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
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError, ()> {
    let this = self.clone();
    let req = req.clone();
    let model = req.model.clone();
    with_ai_span(
      "openai",
      "complete",
      &model,
      retry_transient_ai_http(move || {
        let this = this.clone();
        let req = req.clone();
        Effect::new_async(move |_r| {
          Box::pin(async move {
            req.validate()?;
            let body = json!({
              "model": req.model,
              "messages": OpenAiLanguageModel::build_messages(&req.messages),
              "stream": false,
            });
            let bytes =
              serde_json::to_vec(&body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
            let http_req = this.auth_request(bytes);
            let resp = this
              .client
              .execute(http_req)
              .run(&mut ())
              .await
              .map_err(|e| AiError::Upstream(format!("http: {e}")))?;
            if resp.status == 401 || resp.status == 403 {
              return Err(AiError::Unauthorized);
            }
            if !(200..300).contains(&resp.status) {
              return Err(AiError::from_http_status(
                resp.status,
                String::from_utf8_lossy(&resp.body).to_string(),
              ));
            }
            let parsed: OpenAiCompletionResponse = serde_json::from_slice(&resp.body)
              .map_err(|e| AiError::InvalidJson(e.to_string()))?;
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
          })
        })
      }),
    )
  }

  fn complete_stream(
    &self,
    req: ChatRequest,
  ) -> Effect<id_effect::Stream<CompletionChunk, AiError, ()>, AiError, ()> {
    let this = self.clone();
    Effect::new_async(move |_r| {
      let this = this.clone();
      let req = req.clone();
      Box::pin(async move {
        req.validate()?;
        let body = json!({
          "model": req.model,
          "messages": OpenAiLanguageModel::build_messages(&req.messages),
          "stream": true,
        });
        let bytes = serde_json::to_vec(&body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
        let http_req = this.auth_request(bytes);
        let resp = this
          .client
          .execute_stream(http_req)
          .run(&mut ())
          .await
          .map_err(|e| AiError::Upstream(format!("http: {e}")))?;
        if resp.status == 401 || resp.status == 403 {
          return Err(AiError::Unauthorized);
        }
        if !(200..300).contains(&resp.status) {
          return Err(AiError::from_http_status(
            resp.status,
            "stream failed".to_string(),
          ));
        }
        let byte_chunks = resp
          .body
          .run_collect()
          .run(&mut ())
          .await
          .map_err(|e| AiError::Upstream(format!("stream collect: {e}")))?;
        let text = String::from_utf8_lossy(&byte_chunks).to_string();
        Ok(openai_sse_to_chunk_stream(&text))
      })
    })
  }
}

fn openai_sse_to_chunk_stream(text: &str) -> id_effect::Stream<CompletionChunk, AiError, ()> {
  use id_effect::{Chunk, end_stream, send_chunk, stream_from_channel};
  let (stream, sender) = stream_from_channel::<CompletionChunk, AiError, ()>(16);
  let text = text.to_string();
  std::thread::spawn(move || {
    let mut parser = SseParser::new();
    for msg in parser.feed(&text) {
      let data = msg.data();
      if data == "[DONE]" {
        break;
      }
      if let Ok(chunk) = serde_json::from_str::<OpenAiStreamChunk>(&data) {
        for choice in chunk.choices {
          if let Some(delta) = choice.delta.content
            && !delta.is_empty()
          {
            let c = CompletionChunk { delta, done: false };
            if id_effect::run_blocking(send_chunk(&sender, Chunk::singleton(c)), ()).is_err() {
              return;
            }
          }
        }
      }
    }
    let final_chunk = CompletionChunk {
      delta: String::new(),
      done: true,
    };
    let _ = id_effect::run_blocking(send_chunk(&sender, Chunk::singleton(final_chunk)), ());
    let _ = id_effect::run_blocking(end_stream(sender), ());
  });
  stream
}

/// Register OpenAI [`LanguageModel`] in the capability environment.
pub fn provide_openai_language_model(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Result<ProviderBox, AiError> {
  let model = Arc::new(OpenAiLanguageModel::new(client, &config)?) as Arc<dyn LanguageModel>;
  struct Node {
    model: Arc<dyn LanguageModel>,
  }
  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "ai/openai"
    }
    fn requires(&self) -> &[CapabilityId] {
      &[]
    }
    fn provides(&self) -> CapabilityId {
      Cap::<LanguageModelService>::id()
    }
    fn cap_name(&self) -> &str {
      "LanguageModel"
    }
    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<Cap<LanguageModelService>>(Arc::clone(&self.model));
      Ok(out)
    }
  }
  Ok(ProviderBox(Arc::new(Node { model })))
}
