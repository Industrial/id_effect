//! Ollama embeddings (`POST /api/embed`, features `ollama` and `embeddings`).
//!
//! Ollama needs no API key. Its default base URL is on loopback, which the platform client's
//! SSRF policy refuses until [`AiConfig::http`](crate::AiConfig::http) allows the host
//! (`HttpClientConfig::allow_hosts`).

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HCons, HNil, Layer};
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
use crate::http_util::join_url;
use crate::vendors::Transport;

/// Ollama embeddings client (an [`Embedder`]).
#[derive(Clone)]
pub struct OllamaEmbedder {
  transport: Transport,
  base_url: String,
  limits: EmbeddingLimits,
}

impl fmt::Debug for OllamaEmbedder {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OllamaEmbedder")
      .field("base_url", &self.base_url)
      .field("limits", &self.limits)
      .field("transport", &self.transport)
      .finish()
  }
}

impl OllamaEmbedder {
  /// Create from the platform HTTP client and config
  /// ([`AiConfig::ollama_base_url`](crate::AiConfig::ollama_base_url)), with
  /// [`EmbeddingLimits::default`].
  pub fn new(client: Arc<dyn HttpClient>, config: &AiConfig) -> Self {
    Self {
      transport: Transport::new(client, config),
      base_url: config.ollama_base_url.clone(),
      limits: EmbeddingLimits::default(),
    }
  }

  /// Create with the client [`AiConfig::http_client`] builds (SSRF policy and timeouts of
  /// [`AiConfig::http`]).
  pub fn from_config(config: &AiConfig) -> Self {
    Self::new(config.http_client(), config)
  }

  /// Replaces the limits.
  #[must_use]
  pub fn with_limits(mut self, limits: EmbeddingLimits) -> Self {
    self.limits = limits;
    self
  }

  /// The validated `/api/embed` request for `req`.
  fn embed_request(&self, req: &EmbedRequest) -> Result<HttpRequest, AiError> {
    req.validate()?;
    let mut body = json!({
      "model": req.model,
      "input": req.inputs,
    });
    if let Some(dimensions) = req.dimensions {
      body["dimensions"] = json!(dimensions);
    }
    self.transport.post_json(
      join_url(&self.base_url, "api/embed"),
      Vec::new(),
      &body,
      false,
    )
  }

  fn parse_embeddings(resp: HttpResponse) -> Result<EmbedResponse, AiError> {
    let parsed: OllamaEmbedResponse =
      serde_json::from_slice(&resp.body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
    let embeddings = parsed.embeddings.into_iter().map(Embedding::new).collect();
    let usage = Usage::new(parsed.prompt_eval_count.unwrap_or(0), 0);
    Ok(EmbedResponse::new(embeddings).with_usage(usage))
  }
}

#[derive(Deserialize)]
struct OllamaEmbedResponse {
  embeddings: Vec<Vec<f32>>,
  prompt_eval_count: Option<u32>,
}

impl Embedder for OllamaEmbedder {
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
    observe_embed("ollama", &req, call)
  }
}

/// Register the Ollama [`Embedder`]: a [`Layer`] providing [`EmbedderService`].
pub fn provide_ollama_embedder<E: Send + 'static>(
  client: Arc<dyn HttpClient>,
  config: AiConfig,
) -> Layer<HCons<EmbedderService, HNil>, E> {
  provide_embedder(OllamaEmbedder::new(client, &config))
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

  #[test]
  fn parses_vectors_and_usage() {
    let resp = OllamaEmbedder::parse_embeddings(response(
      r#"{"model":"m","embeddings":[[1.0,2.0]],"prompt_eval_count":4}"#,
    ))
    .expect("parse");
    assert_eq!(resp.embeddings[0].as_slice(), &[1.0, 2.0]);
    assert_eq!(resp.usage, Usage::new(4, 0));
    let resp = OllamaEmbedder::parse_embeddings(response(r#"{"embeddings":[]}"#)).expect("parse");
    assert_eq!(resp.usage, Usage::default());
    assert!(matches!(
      OllamaEmbedder::parse_embeddings(response("{}")),
      Err(AiError::InvalidJson(_))
    ));
  }

  #[test]
  fn limits_and_debug() {
    let embedder = OllamaEmbedder::from_config(&AiConfig::default())
      .with_limits(EmbeddingLimits::default().with_max_batch_size(3));
    assert_eq!(embedder.limits().max_batch_size, 3);
    assert!(format!("{embedder:?}").contains("localhost:11434"));
  }
}
