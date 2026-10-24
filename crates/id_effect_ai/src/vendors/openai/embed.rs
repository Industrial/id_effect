//! OpenAI embeddings (`POST /v1/embeddings`, features `openai` and `embeddings`), also for
//! OpenAI-compatible servers.
//!
//! Vectors are requested with `encoding_format: "base64"` (little-endian `f32`s), a quarter of
//! the JSON-float size, so a batch stays well inside
//! [`AiLimits::max_response_bytes`](crate::AiLimits::max_response_bytes); a server that ignores
//! the parameter and answers with float arrays is decoded too. The API key is a [`Secret`],
//! exposed only to build the `Authorization` header.

use std::fmt;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use id_effect::{Effect, HCons, HNil, Layer};
use id_effect_config::Secret;
use id_effect_platform::http::{HttpClient, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::chat::Usage;
use crate::config::AiConfig;
use crate::embeddings::{
  EmbedRequest, EmbedResponse, Embedder, EmbedderService, Embedding, EmbeddingError,
  EmbeddingLimits, observe_embed, provide_embedder,
};
use crate::error::AiError;
use crate::http_util::{bearer_header, join_url};
use crate::vendors::Transport;

/// OpenAI embeddings client (an [`Embedder`]). `Debug` never prints the API key.
#[derive(Clone)]
pub struct OpenAiEmbedder {
  transport: Transport,
  api_key: Secret<String>,
  base_url: String,
  limits: EmbeddingLimits,
}

impl fmt::Debug for OpenAiEmbedder {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OpenAiEmbedder")
      .field("base_url", &self.base_url)
      .field("api_key", &self.api_key)
      .field("limits", &self.limits)
      .field("transport", &self.transport)
      .finish()
  }
}

impl OpenAiEmbedder {
  /// The most inputs the embeddings endpoint accepts in one call (2048): the batch size is
  /// never set above it.
  pub const MAX_BATCH_SIZE: usize = 2048;

  /// Create from the platform HTTP client and config (requires the API key), with
  /// [`EmbeddingLimits::default`].
  pub fn new(client: Arc<dyn HttpClient>, config: &AiConfig) -> Result<Self, AiError> {
    let api_key = config.require_openai_key()?.clone();
    Ok(Self {
      transport: Transport::new(client, config),
      api_key,
      base_url: config.openai_base_url.clone(),
      limits: EmbeddingLimits::default(),
    })
  }

  /// Create with the client [`AiConfig::http_client`] builds (SSRF policy and timeouts of
  /// [`AiConfig::http`]).
  pub fn from_config(config: &AiConfig) -> Result<Self, AiError> {
    Self::new(config.http_client(), config)
  }

  /// Replaces the limits; the batch size is capped at [`Self::MAX_BATCH_SIZE`].
  #[must_use]
  pub fn with_limits(mut self, limits: EmbeddingLimits) -> Self {
    self.limits = limits.with_max_batch_size(limits.max_batch_size.min(Self::MAX_BATCH_SIZE));
    self
  }

  /// The validated embeddings request for `req`.
  fn embed_request(&self, req: &EmbedRequest) -> Result<HttpRequest, AiError> {
    req.validate()?;
    let mut body = json!({
      "model": req.model,
      "input": req.inputs,
      "encoding_format": "base64",
    });
    if let Some(dimensions) = req.dimensions {
      body["dimensions"] = json!(dimensions);
    }
    self.transport.post_json(
      join_url(&self.base_url, "v1/embeddings"),
      vec![bearer_header(self.api_key.expose())],
      &body,
      false,
    )
  }

  fn parse_embeddings(resp: HttpResponse) -> Result<EmbedResponse, AiError> {
    let parsed: OpenAiEmbeddingResponse =
      serde_json::from_slice(&resp.body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
    let mut data = parsed.data;
    data.sort_by_key(|d| d.index);
    let embeddings = data
      .into_iter()
      .map(|d| d.embedding.decode())
      .collect::<Result<Vec<_>, _>>()?;
    let input_tokens = parsed.usage.and_then(|u| u.prompt_tokens).unwrap_or(0);
    Ok(EmbedResponse::new(embeddings).with_usage(Usage::new(input_tokens, 0)))
  }
}

#[derive(Deserialize)]
struct OpenAiEmbeddingResponse {
  data: Vec<OpenAiEmbeddingDatum>,
  usage: Option<OpenAiEmbeddingUsage>,
}

#[derive(Deserialize)]
struct OpenAiEmbeddingDatum {
  #[serde(default)]
  index: usize,
  embedding: OpenAiVector,
}

/// A vector as `encoding_format: "base64"` sends it, or as float JSON.
#[derive(Deserialize)]
#[serde(untagged)]
enum OpenAiVector {
  Floats(Vec<f32>),
  Base64(String),
}

impl OpenAiVector {
  fn decode(self) -> Result<Embedding, AiError> {
    match self {
      Self::Floats(v) => Ok(Embedding::new(v)),
      Self::Base64(s) => {
        let bytes = STANDARD
          .decode(s.as_bytes())
          .map_err(|e| AiError::InvalidJson(format!("embedding base64: {e}")))?;
        let (floats, rest) = bytes.as_chunks::<4>();
        if !rest.is_empty() {
          return Err(AiError::InvalidJson(format!(
            "embedding base64: {} bytes is not a whole number of f32s",
            bytes.len()
          )));
        }
        Ok(Embedding::new(
          floats.iter().map(|b| f32::from_le_bytes(*b)).collect(),
        ))
      }
    }
  }
}

#[derive(Deserialize)]
struct OpenAiEmbeddingUsage {
  prompt_tokens: Option<u32>,
}

impl Embedder for OpenAiEmbedder {
  fn limits(&self) -> EmbeddingLimits {
    self.limits
  }

  fn embed_batch(&self, req: EmbedRequest) -> Effect<EmbedResponse, EmbeddingError> {
    let this = self.clone();
    let body = req.clone();
    let call = self
      .transport
      .call(
        Arc::new(move || this.embed_request(&body)),
        Self::parse_embeddings,
      )
      .map_err(EmbeddingError::from);
    observe_embed("openai", &req, call)
  }
}

/// Register the OpenAI [`Embedder`]: a [`Layer`] providing [`EmbedderService`].
pub fn provide_openai_embedder<E: Send + 'static>(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Result<Layer<HCons<EmbedderService, HNil>, E>, AiError> {
  Ok(provide_embedder(OpenAiEmbedder::new(client, &config)?))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn response(body: &str) -> HttpResponse {
    HttpResponse {
      status: 200,
      headers: vec![],
      body: body.as_bytes().to_vec(),
    }
  }

  fn b64(v: &[f32]) -> String {
    STANDARD.encode(v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
  }

  #[test]
  fn parses_base64_and_floats_in_index_order() {
    let body = format!(
      r#"{{"data":[{{"index":1,"embedding":[3.0,4.0]}},{{"index":0,"embedding":"{}"}}],"usage":{{"prompt_tokens":9}}}}"#,
      b64(&[1.0, 2.0])
    );
    let resp = OpenAiEmbedder::parse_embeddings(response(&body)).expect("parse");
    assert_eq!(resp.embeddings[0].as_slice(), &[1.0, 2.0]);
    assert_eq!(resp.embeddings[1].as_slice(), &[3.0, 4.0]);
    assert_eq!(resp.usage, Usage::new(9, 0));
    let resp = OpenAiEmbedder::parse_embeddings(response(r#"{"data":[]}"#)).expect("parse");
    assert_eq!(resp.usage, Usage::default());
  }

  #[test]
  fn bad_vectors_are_invalid_json() {
    for body in [
      r#"{"data":[{"embedding":"!!"}]}"#.to_string(),
      format!(
        r#"{{"data":[{{"embedding":"{}"}}]}}"#,
        STANDARD.encode([1u8, 2, 3])
      ),
      "{".to_string(),
    ] {
      assert!(matches!(
        OpenAiEmbedder::parse_embeddings(response(&body)),
        Err(AiError::InvalidJson(_))
      ));
    }
  }

  #[test]
  fn limits_are_capped_and_key_redacted() {
    let cfg = AiConfig::default().with_openai_api_key("sk-embed-secret");
    let embedder = OpenAiEmbedder::from_config(&cfg)
      .expect("embedder")
      .with_limits(EmbeddingLimits::default().with_max_batch_size(1_000_000));
    assert_eq!(
      embedder.limits().max_batch_size,
      OpenAiEmbedder::MAX_BATCH_SIZE
    );
    let printed = format!("{embedder:?}");
    assert!(!printed.contains("sk-embed-secret"), "{printed}");
    assert!(OpenAiEmbedder::from_config(&AiConfig::default()).is_err());
    assert!(provide_openai_embedder::<AiError>(cfg.http_client(), AiConfig::default()).is_err());
  }
}
