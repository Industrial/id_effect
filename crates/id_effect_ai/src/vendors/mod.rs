//! HTTP vendor adapters implementing [`LanguageModel`](crate::chat::LanguageModel).
//!
//! One module per vendor, each with `chat` (and, with feature `embeddings`, `embed`). Every
//! adapter sends through id_effect_platform's [`HttpClient`](id_effect_platform::http::HttpClient)
//! (no adapter builds its own client), so the platform's SSRF policy, redirect limit and body
//! caps apply to every provider base URL; each request also carries
//! [`AiLimits::request_timeout`](crate::AiLimits::request_timeout) and
//! [`AiLimits::max_response_bytes`](crate::AiLimits::max_response_bytes) (or
//! [`max_stream_bytes`](crate::AiLimits::max_stream_bytes) for a stream), and transient
//! statuses (429, 502, 503, 504, 529) are retried with backoff.

#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "ollama")]
pub mod ollama;
#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "anthropic")]
pub use anthropic::{AnthropicLanguageModel, provide_anthropic_language_model};
#[cfg(feature = "openai")]
pub use openai::{OpenAiLanguageModel, provide_openai_language_model};

#[cfg(any(feature = "openai", feature = "anthropic", feature = "ollama"))]
#[cfg_attr(
  not(any(feature = "openai", feature = "anthropic")),
  allow(unused_imports)
)]
pub(crate) use transport::Transport;

/// Shared by the chat adapters and, with feature `embeddings`, the embedding adapters.
#[cfg(any(feature = "openai", feature = "anthropic", feature = "ollama"))]
#[cfg_attr(not(any(feature = "openai", feature = "anthropic")), allow(dead_code))]
mod transport {
  use std::fmt;
  use std::sync::Arc;

  use id_effect::{Effect, Stream, fail, suspend};
  use id_effect_platform::http::{HttpClient, HttpMethod, HttpRequest, HttpResponse};

  use crate::config::{AiConfig, AiLimits};
  use crate::error::AiError;
  use crate::http_util::{check_status, from_result};
  use crate::retry::retry_transient_ai_http;
  use crate::stream::{CompletionChunk, SseStep, sse_chunk_stream};

  /// Builds the HTTP request of one attempt (called again on every retry).
  pub(crate) type MakeRequest = Arc<dyn Fn() -> Result<HttpRequest, AiError> + Send + Sync>;

  /// The request plumbing every chat adapter shares: the platform client, the limits, and
  /// whether prompts may be recorded.
  #[derive(Clone)]
  pub(crate) struct Transport {
    pub(crate) client: Arc<dyn HttpClient>,
    pub(crate) limits: AiLimits,
    pub(crate) log_prompts: bool,
  }

  impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("Transport")
        .field("limits", &self.limits)
        .field("log_prompts", &self.log_prompts)
        .finish_non_exhaustive()
    }
  }

  impl Transport {
    pub(crate) fn new(client: Arc<dyn HttpClient>, config: &AiConfig) -> Self {
      Self {
        client,
        limits: config.limits,
        log_prompts: config.logs_prompts(),
      }
    }

    /// A JSON `POST` to `url` with `headers`, the request timeout and the response cap.
    pub(crate) fn post_json(
      &self,
      url: String,
      mut headers: Vec<(String, String)>,
      body: &serde_json::Value,
      stream: bool,
    ) -> Result<HttpRequest, AiError> {
      let bytes = serde_json::to_vec(body).map_err(|e| AiError::InvalidJson(e.to_string()))?;
      headers.push(("Content-Type".to_string(), "application/json".to_string()));
      if stream {
        headers.push(("Accept".to_string(), "text/event-stream".to_string()));
      }
      let max_body = if stream {
        self.limits.max_stream_bytes
      } else {
        self.limits.max_response_bytes
      };
      Ok(HttpRequest {
        method: HttpMethod::Post,
        url,
        headers,
        body: Some(bytes),
        timeout: Some(self.limits.request_timeout),
        max_body_bytes: Some(max_body),
      })
    }

    /// Sends the request `make` builds and parses a 2xx response with `parse`; transient
    /// statuses are retried ([`retry_transient_ai_http`]).
    pub(crate) fn call<A>(
      &self,
      make: MakeRequest,
      parse: fn(HttpResponse) -> Result<A, AiError>,
    ) -> Effect<A, AiError>
    where
      A: Send + 'static,
    {
      let client = Arc::clone(&self.client);
      retry_transient_ai_http(move || {
        let client = Arc::clone(&client);
        let make = Arc::clone(&make);
        suspend(move || match make() {
          Ok(req) => client
            .execute(req)
            .map_err(AiError::from)
            .flat_map(move |resp| {
              from_result(
                check_status(resp.status, || {
                  String::from_utf8_lossy(&resp.body).into_owned()
                })
                .and_then(|()| parse(resp)),
              )
            }),
          Err(e) => fail(e),
        })
      })
    }

    /// Opens the SSE stream `make` builds (transient statuses retried) and decodes it
    /// incrementally with `decode` under the stream limits.
    pub(crate) fn open_stream(
      &self,
      make: MakeRequest,
      decode: fn(&crate::stream::SseMessage) -> SseStep,
    ) -> Effect<Stream<CompletionChunk, AiError>, AiError> {
      let client = Arc::clone(&self.client);
      let limits = self.limits;
      retry_transient_ai_http(move || {
        let client = Arc::clone(&client);
        let make = Arc::clone(&make);
        suspend(move || match make() {
          Ok(req) => client
            .execute_stream(req)
            .map_err(AiError::from)
            .flat_map(move |resp| {
              if let Err(e) = check_status(resp.status, || "stream refused".to_string()) {
                return fail(e);
              }
              id_effect::succeed(sse_chunk_stream(resp.body, &limits, decode))
            }),
          Err(e) => fail(e),
        })
      })
    }
  }
}
