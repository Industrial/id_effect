//! Tracing span helpers for AI vendor calls (ai.llm #7).
//!
//! Every vendor call runs inside an `ai.request` span with the attributes `ai.vendor`,
//! `ai.operation` and `ai.model`; a buffered completion adds the annotations
//! `ai.usage.input_tokens`, `ai.usage.output_tokens` and `ai.stop_reason`. Prompts and
//! completions are never put on a span unless the config took
//! [`AiConfig::dangerous_log_prompts`](crate::AiConfig::dangerous_log_prompts) (ADR 0013 R3).

use id_effect::Effect;

use crate::error::AiError;

/// The span name of every vendor call.
pub const AI_SPAN: &str = "ai.request";

/// Wrap an AI vendor effect in an [`AI_SPAN`] span with the attributes `ai.vendor`,
/// `ai.operation` and `ai.model` (no prompt content).
pub fn with_ai_request_span<A, E, R>(
  vendor: &'static str,
  operation: &'static str,
  model: &str,
  effect: Effect<A, E, R>,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  effect.with_span_attrs(
    AI_SPAN,
    [
      ("ai.vendor", vendor.to_string()),
      ("ai.operation", operation.to_string()),
      ("ai.model", model.to_string()),
    ],
  )
}

/// Same as [`with_ai_request_span`] for [`AiError`] programs that need no capabilities.
pub fn with_ai_span<A>(
  vendor: &'static str,
  operation: &'static str,
  model: &str,
  effect: Effect<A, AiError>,
) -> Effect<A, AiError>
where
  A: Send + 'static,
{
  with_ai_request_span(vendor, operation, model, effect)
}

#[cfg(any(feature = "openai", feature = "anthropic"))]
pub(crate) use chat_spans::{observe_chat, observe_chat_stream};

#[cfg(any(feature = "openai", feature = "anthropic"))]
mod chat_spans {
  use id_effect::{Effect, Stream, annotate_current_span, succeed};

  use super::with_ai_span;
  use crate::chat::{ChatRequest, ChatResponse};
  use crate::error::AiError;
  use crate::stream::CompletionChunk;

  /// Adds each `(key, value)` to the current span, in order.
  fn annotate_all(pairs: Vec<(&'static str, String)>) -> Effect<(), AiError> {
    pairs.into_iter().fold(succeed(()), |acc, (key, value)| {
      acc.flat_map(move |()| annotate_current_span(key, value))
    })
  }

  /// The prompt as recorded under `dangerous_log_prompts`: one `role: content` line per message.
  fn render_prompt(req: &ChatRequest) -> String {
    req
      .messages
      .iter()
      .map(|m| format!("{:?}: {}", m.role, m.content))
      .collect::<Vec<_>>()
      .join("\n")
  }

  fn prompt_note(req: &ChatRequest, log_prompts: bool) -> Vec<(&'static str, String)> {
    if log_prompts {
      vec![("ai.prompt", render_prompt(req))]
    } else {
      Vec::new()
    }
  }

  /// `call` inside the vendor's span, annotated with usage and stop reason (and, only when
  /// `log_prompts`, the prompt and the completion).
  pub(crate) fn observe_chat(
    vendor: &'static str,
    req: &ChatRequest,
    log_prompts: bool,
    call: Effect<ChatResponse, AiError>,
  ) -> Effect<ChatResponse, AiError> {
    let effect = annotate_all(prompt_note(req, log_prompts))
      .flat_map(move |()| call)
      .flat_map(move |resp| {
        let mut notes = vec![
          ("ai.usage.input_tokens", resp.usage.input_tokens.to_string()),
          (
            "ai.usage.output_tokens",
            resp.usage.output_tokens.to_string(),
          ),
        ];
        if let Some(reason) = &resp.stop_reason {
          notes.push(("ai.stop_reason", reason.clone()));
        }
        if log_prompts {
          notes.push(("ai.completion", resp.content.clone()));
        }
        annotate_all(notes).map(move |()| resp)
      });
    with_ai_span(vendor, "chat", &req.model, effect)
  }

  /// Opening a stream inside the vendor's span (with the prompt only when `log_prompts`).
  pub(crate) fn observe_chat_stream(
    vendor: &'static str,
    req: &ChatRequest,
    log_prompts: bool,
    open: Effect<Stream<CompletionChunk, AiError>, AiError>,
  ) -> Effect<Stream<CompletionChunk, AiError>, AiError> {
    let effect = annotate_all(prompt_note(req, log_prompts)).flat_map(move |()| open);
    with_ai_span(vendor, "chat_stream", &req.model, effect)
  }
}
