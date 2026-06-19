//! Anthropic Messages API (`/v1/messages`).

use std::sync::Arc;

use id_effect::kernel::Effect;
use id_effect::{CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode};
use id_effect_platform::http::{HttpClient, HttpMethod, HttpRequest};
use serde::Deserialize;
use serde_json::json;

use crate::config::AiConfig;
use crate::error::AiError;
use crate::http_util::join_url;
use crate::model::{ChatMessage, ChatRequest, ChatResponse, ChatRole, LanguageModel};
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

impl LanguageModel for AnthropicLanguageModel {
  fn complete(&self, req: ChatRequest) -> Effect<ChatResponse, AiError, ()> {
    let this = self.clone();
    let req = req.clone();
    let model = req.model.clone();
    with_ai_span(
      "anthropic",
      "complete",
      &model,
      retry_transient_ai_http(move || {
        let this = this.clone();
        let req = req.clone();
        Effect::new_async(move |_r| {
          Box::pin(async move {
            req.validate()?;
            let (system, msgs) = AnthropicLanguageModel::split_system(&req.messages);
            let mut body = json!({
              "model": req.model,
              "max_tokens": this.max_tokens,
              "messages": msgs,
            });
            if let Some(s) = system {
              body["system"] = json!(s);
            }
            let bytes =
              serde_json::to_vec(&body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
            let http_req = this.auth_request(bytes, false);
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
            let parsed: AnthropicResponse = serde_json::from_slice(&resp.body)
              .map_err(|e| AiError::InvalidJson(e.to_string()))?;
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
        let (system, msgs) = AnthropicLanguageModel::split_system(&req.messages);
        let mut body = json!({
          "model": req.model,
          "max_tokens": this.max_tokens,
          "stream": true,
          "messages": msgs,
        });
        if let Some(s) = system {
          body["system"] = json!(s);
        }
        let bytes = serde_json::to_vec(&body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
        let http_req = this.auth_request(bytes, true);
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
        Ok(anthropic_sse_to_chunk_stream(&text))
      })
    })
  }
}

fn anthropic_sse_to_chunk_stream(text: &str) -> id_effect::Stream<CompletionChunk, AiError, ()> {
  use id_effect::{Chunk, end_stream, send_chunk, stream_from_channel};
  let (stream, sender) = stream_from_channel::<CompletionChunk, AiError, ()>(16);
  let text = text.to_string();
  std::thread::spawn(move || {
    let mut parser = SseParser::new();
    for msg in parser.feed(&text) {
      let data = msg.data();
      if let Ok(ev) = serde_json::from_str::<AnthropicStreamEvent>(&data)
        && ev.kind == "content_block_delta"
        && let Some(delta) = ev.delta.and_then(|d| d.text)
        && !delta.is_empty()
      {
        let c = CompletionChunk { delta, done: false };
        if id_effect::run_blocking(send_chunk(&sender, Chunk::singleton(c)), ()).is_err() {
          return;
        }
      }
    }
    let _ = id_effect::run_blocking(
      send_chunk(
        &sender,
        Chunk::singleton(CompletionChunk {
          delta: String::new(),
          done: true,
        }),
      ),
      (),
    );
    let _ = id_effect::run_blocking(end_stream(sender), ());
  });
  stream
}

/// Register Anthropic [`LanguageModel`].
pub fn provide_anthropic_language_model(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Result<ProviderBox, AiError> {
  let model = Arc::new(AnthropicLanguageModel::new(client, &config)?) as Arc<dyn LanguageModel>;
  struct Node {
    model: Arc<dyn LanguageModel>,
  }
  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "ai/anthropic"
    }
    fn requires(&self) -> &[CapabilityId] {
      &[]
    }
    fn provides(&self) -> CapabilityId {
      crate::model::LanguageModelKey::id()
    }
    fn cap_name(&self) -> &str {
      "LanguageModelKey"
    }
    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<crate::model::LanguageModelKey>(Arc::clone(&self.model));
      Ok(out)
    }
  }
  Ok(ProviderBox(Arc::new(Node { model })))
}
