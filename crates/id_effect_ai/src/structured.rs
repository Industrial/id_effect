//! Structured output (`feature = "tools"`): ask the model for a JSON value of type `T` and decode
//! the reply through `T`'s `Schema`, so refinements run and a reply that does not fit is a typed
//! [`StructuredError`], never a half-valid `T`.
//!
//! [`complete_structured`] adds one system message with `T`'s JSON Schema (exported through
//! `id_effect::schema::json_schema_for`), reads the reply as JSON (a surrounding Markdown code
//! fence is tolerated) and decodes it with [`Schema::decode_unknown_all`], which reports every
//! issue with its path. When decoding fails it asks once more, telling the model what was wrong:
//! at most [`MAX_REPAIR_RETRIES`] repair retry, configurable down to none
//! ([`StructuredOptions`]); after that the failure is [`StructuredError::Decode`].
//!
//! ```
//! use id_effect::{HCons, HNil, Schema};
//! use id_effect_ai::structured::complete_structured;
//! use id_effect_ai::testing::ScriptedModel;
//! use id_effect_ai::{AiError, ChatRequest, LanguageModelService};
//!
//! #[derive(Clone, Debug, Schema)]
//! struct Invoice { number: String, total_cents: i64 }
//!
//! let model = ScriptedModel::new().reply(r#"{"number": "INV-7", "total_cents": 1250}"#);
//! let invoice = complete_structured::<Invoice, HCons<LanguageModelService, HNil>, _>(
//!   ChatRequest::user("m", "Extract the invoice"),
//! )
//! .provide_layer(model.layer::<AiError>())
//! .block_on()
//! .into_result()
//! .expect("decoded");
//! assert_eq!(invoice.total_cents, 1250);
//! ```
//!
//! [`Schema::decode_unknown_all`]: id_effect::Schema::decode_unknown_all

use id_effect::schema::{json_schema_for, unknown_from_serde_json};
use id_effect::{Effect, HList, HasSchema, Selector, Unknown, fail, need, succeed};
use thiserror::Error;

use crate::chat::{ChatMessage, ChatRequest, ChatResponse, LanguageModelService, Usage};
use crate::error::{AiError, truncate_detail};

/// Upper bound on repair retries: one (the model gets one chance to correct its reply).
pub const MAX_REPAIR_RETRIES: u8 = 1;

/// Why structured output failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum StructuredError {
  /// The model call failed.
  #[error(transparent)]
  Model(#[from] AiError),
  /// The reply did not decode to the requested type after `attempts` model calls. `message`
  /// lists the JSON or schema issues (with paths, truncated); the reply text itself is not kept,
  /// so the error can be logged without the completion.
  #[error("structured output did not decode after {attempts} attempt(s): {message}")]
  Decode {
    /// The decode issues of the last attempt.
    message: String,
    /// Model calls made (1 plus the repair retries used).
    attempts: u8,
  },
}

/// Options of [`complete_structured_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredOptions {
  repair_retries: u8,
}

impl Default for StructuredOptions {
  /// One repair retry.
  fn default() -> Self {
    Self {
      repair_retries: MAX_REPAIR_RETRIES,
    }
  }
}

impl StructuredOptions {
  /// Sets the repair retries, capped at [`MAX_REPAIR_RETRIES`].
  #[must_use]
  pub fn with_repair_retries(mut self, n: u8) -> Self {
    self.repair_retries = n.min(MAX_REPAIR_RETRIES);
    self
  }

  /// No repair retry: the first decode failure is returned.
  #[must_use]
  pub fn without_repair(self) -> Self {
    self.with_repair_retries(0)
  }

  /// The repair retries allowed.
  pub fn repair_retries(&self) -> u8 {
    self.repair_retries
  }
}

/// A decoded reply with what it cost.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Structured<T> {
  /// The decoded value.
  pub value: T,
  /// Model calls made (1, or 2 after a repair).
  pub attempts: u8,
  /// Token usage summed over the attempts.
  pub usage: Usage,
}

/// The instruction added to the request: reply with JSON matching `T`'s JSON Schema.
pub fn schema_instruction<T: HasSchema>() -> String {
  format!(
    "Reply with only a JSON value, without prose or a code fence, that matches this JSON \
     Schema:\n{}",
    json_schema_for::<T>()
  )
}

/// Decodes a reply `text` as a `T`: the JSON value in it (surrounding whitespace and a Markdown
/// code fence are ignored) through `T`'s schema. The error lists every issue; it is the text of
/// [`StructuredError::Decode`] and of the repair prompt.
pub fn decode_reply<T>(text: &str) -> Result<T, String>
where
  T: HasSchema<A = T, I = Unknown>,
{
  let json: serde_json::Value = serde_json::from_str(strip_code_fence(text))
    .map_err(|e| truncate_detail(format!("the reply is not JSON: {e}")))?;
  T::schema()
    .decode_unknown_all(&unknown_from_serde_json(json))
    .map_err(|errors| truncate_detail(errors.to_string()))
}

/// `text` without surrounding whitespace and a surrounding ```` ``` ```` / ```` ```json ```` fence.
fn strip_code_fence(text: &str) -> &str {
  let t = text.trim();
  let Some(rest) = t.strip_prefix("```") else {
    return t;
  };
  let body = rest.split_once('\n').map_or("", |(_, body)| body);
  body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

/// Asks `model` for a `T` (see the module docs) with `options`.
pub fn complete_structured_with<T>(
  model: LanguageModelService,
  req: ChatRequest,
  options: StructuredOptions,
) -> Effect<Structured<T>, StructuredError>
where
  T: HasSchema<A = T, I = Unknown> + Send + 'static,
{
  let mut req = req;
  req
    .messages
    .insert(0, ChatMessage::system(schema_instruction::<T>()));
  attempt(model, req, options.repair_retries, 1, Usage::default())
}

/// One model call; on a decode failure with retries left, a repair call.
fn attempt<T>(
  model: LanguageModelService,
  req: ChatRequest,
  retries_left: u8,
  attempts: u8,
  usage: Usage,
) -> Effect<Structured<T>, StructuredError>
where
  T: HasSchema<A = T, I = Unknown> + Send + 'static,
{
  model
    .complete(req.clone())
    .map_err(StructuredError::Model)
    .flat_map(move |resp: ChatResponse| {
      let usage = Usage::new(
        usage.input_tokens.saturating_add(resp.usage.input_tokens),
        usage.output_tokens.saturating_add(resp.usage.output_tokens),
      );
      match decode_reply::<T>(&resp.content) {
        Ok(value) => succeed(Structured {
          value,
          attempts,
          usage,
        }),
        Err(message) if retries_left > 0 => {
          let mut req = req;
          req.messages.push(ChatMessage::assistant(resp.content));
          req.messages.push(ChatMessage::user(format!(
            "That reply does not match the schema: {message}\nReply again with only the \
             corrected JSON value."
          )));
          attempt(model, req, retries_left - 1, attempts + 1, usage)
        }
        Err(message) => fail(StructuredError::Decode { message, attempts }),
      }
    })
}

/// Asks the [`LanguageModelService`] in the environment for a `T`, with one repair retry.
pub fn complete_structured<T, R, I>(req: ChatRequest) -> Effect<T, StructuredError, R>
where
  T: HasSchema<A = T, I = Unknown> + Send + 'static,
  R: Selector<LanguageModelService, I> + HList,
{
  need::<LanguageModelService>().flat_map(move |model| {
    complete_structured_with::<T>(
      LanguageModelService::clone(&model),
      req,
      StructuredOptions::default(),
    )
    .map(|s| s.value)
    .narrow()
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn code_fences_are_stripped() {
    assert_eq!(strip_code_fence("  {\"a\":1} "), "{\"a\":1}");
    assert_eq!(strip_code_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
    assert_eq!(strip_code_fence("```\n[1]\n```\n"), "[1]");
    assert_eq!(strip_code_fence("```\n[1]"), "[1]");
    assert_eq!(strip_code_fence("```"), "");
  }

  #[test]
  fn options_cap_repairs_at_one() {
    assert_eq!(StructuredOptions::default().repair_retries(), 1);
    assert_eq!(
      StructuredOptions::default()
        .with_repair_retries(9)
        .repair_retries(),
      MAX_REPAIR_RETRIES
    );
    assert_eq!(
      StructuredOptions::default()
        .without_repair()
        .repair_retries(),
      0
    );
  }
}
