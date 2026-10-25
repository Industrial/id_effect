//! Typed tool calling (`feature = "tools"`): tools whose input and output have a `Schema`, the
//! `ToolSet` a call may use, and the deny-by-default rules of ADR 0015 D9 (only registered
//! tools run, arguments are decoded before a handler sees them, write tools need an explicit
//! policy, and every loop is bounded).
//!
//! - [`Tool<I, O>`](Tool): a named handler `I -> Effect<O, E>` where `I` and `O` implement
//!   [`HasSchema`] (`#[derive(Schema)]`). Its parameters are the JSON Schema of `I`, exported
//!   through `id_effect::schema::json_schema_for` ([`Tool::parameters_schema`]), so what the
//!   model is told and what the handler accepts come from one definition.
//! - [`ToolSet`]: the tools one call may use. [`ToolSet::definitions`] goes into
//!   [`ChatRequest::tools`](crate::ChatRequest::tools) (both vendors' formats are written by the
//!   adapters); [`ToolSet::invoke`] runs a model's [`ToolCall`] under the D9 rules.
//! - [`ToolPolicy`]: tools declared [`ToolEffects::Write`] ([`Tool::writes`]) run only under
//!   [`ToolPolicy::allow_writes`].
//! - [`decode_arguments`]: the argument decoder every call goes through (public for the
//!   `ai_tool_arguments` fuzz target).
//!
//! A refused call never runs anything: an unknown name, arguments that do not decode, a write
//! tool without the policy, or an output over the cap each become a [`ToolError`] that the agent
//! loop ([`crate::agent::ToolLoop`]) feeds back to the model as the tool's result.
//!
//! ```
//! use id_effect::{Effect, Schema, succeed};
//! use id_effect_ai::ToolCall;
//! use id_effect_ai::tools::{Tool, ToolError, ToolPolicy, ToolSet};
//!
//! #[derive(Clone, Debug, Schema)]
//! struct Add { a: i64, b: i64 }
//! #[derive(Clone, Debug, Schema)]
//! struct Sum { sum: i64 }
//!
//! let add = Tool::new("add", "Adds two integers", |i: Add| -> Effect<Sum, String> {
//!   succeed(Sum { sum: i.a + i.b })
//! });
//! let tools = ToolSet::new().with(add).expect("valid tool");
//! let policy = ToolPolicy::default();
//!
//! let call = ToolCall::new("c1", "add", serde_json::json!({ "a": 2, "b": 3 }));
//! let out = tools.invoke(&call, &policy, 64 * 1024).block_on().into_result().expect("runs");
//! assert_eq!(out.value, serde_json::json!({ "sum": 5 }));
//!
//! let unknown = ToolCall::new("c2", "rm_rf", serde_json::json!({}));
//! let refused = tools.invoke(&unknown, &policy, 64 * 1024).block_on().into_result().unwrap_err();
//! assert!(matches!(refused.fail_option(), Some(ToolError::Unregistered { .. })));
//! ```

mod set;
mod tool;

use id_effect::schema::unknown_from_serde_json;
use id_effect::{HasSchema, Unknown};
use thiserror::Error;

use crate::error::truncate_detail;

pub use crate::chat::{ToolCall, ToolDefinition};
pub use set::{ToolOutput, ToolSet};
pub use tool::Tool;

/// Default cap on one tool's output (its JSON text), 64 KiB (ADR 0015 D9).
pub const DEFAULT_MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

/// Longest tool name both vendors accept.
pub const MAX_TOOL_NAME_LEN: usize = 64;

/// What running a tool may change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ToolEffects {
  /// Reads only (the default).
  #[default]
  Read,
  /// Writes (changes state outside the program): runs only under
  /// [`ToolPolicy::allow_writes`].
  Write,
}

/// Which tools of a [`ToolSet`] a call may run. The default denies every
/// [`ToolEffects::Write`] tool (ADR 0013 R1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolPolicy {
  allow_writes: bool,
}

impl ToolPolicy {
  /// The default policy: read tools only.
  pub fn read_only() -> Self {
    Self::default()
  }

  /// Also allows tools declared [`ToolEffects::Write`]. Grant it per call, for a set whose
  /// write tools the caller has reviewed: a model's choice of tool is untrusted input.
  #[must_use]
  pub fn allow_writes(mut self) -> Self {
    self.allow_writes = true;
    self
  }

  /// Whether write tools may run.
  pub fn writes_allowed(&self) -> bool {
    self.allow_writes
  }

  /// Whether a tool with `effects` may run under this policy.
  pub fn permits(&self, effects: ToolEffects) -> bool {
    match effects {
      ToolEffects::Read => true,
      ToolEffects::Write => self.allow_writes,
    }
  }
}

/// Why a tool call did not produce an output. Every variant is a refusal the agent loop feeds
/// back to the model; none aborts the loop.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ToolError {
  /// The model named a tool that is not registered on the call's [`ToolSet`]; nothing ran.
  #[error("tool `{name}` is not available")]
  Unregistered {
    /// The name the model asked for (truncated).
    name: String,
  },
  /// The arguments did not decode through the tool's input `Schema`; the handler did not run.
  #[error("invalid arguments for tool `{tool}`: {message}")]
  InvalidArguments {
    /// The tool.
    tool: String,
    /// Every decode issue, with its path (truncated).
    message: String,
  },
  /// The tool writes and the call's [`ToolPolicy`] does not allow writes; nothing ran.
  #[error("tool `{tool}` changes state and is not allowed for this call")]
  WriteDenied {
    /// The tool.
    tool: String,
  },
  /// The handler failed (its error's `Display`, truncated).
  #[error("tool `{tool}` failed: {message}")]
  Failed {
    /// The tool.
    tool: String,
    /// The handler's error.
    message: String,
  },
  /// The output's JSON text is larger than the cap (ADR 0013 R4).
  #[error("output of tool `{tool}` exceeds {max} bytes")]
  OutputTooLarge {
    /// The tool.
    tool: String,
    /// The cap in bytes.
    max: usize,
  },
  /// A tool that cannot be registered: a bad or duplicate name, or input that is not a JSON
  /// object.
  #[error("invalid tool definition: {0}")]
  InvalidDefinition(String),
}

impl ToolError {
  /// The refusal as the JSON text fed back to the model: `{"error": "<message>"}`.
  pub fn to_model_text(&self) -> String {
    serde_json::json!({ "error": self.to_string() }).to_string()
  }
}

/// Decodes a model's `arguments` for tool `tool` through `I`'s `Schema` (refinements run), so a
/// handler only ever sees a valid `I`. Every issue is reported with its path in
/// [`ToolError::InvalidArguments`].
///
/// ```
/// use id_effect::Schema;
/// use id_effect_ai::tools::{ToolError, decode_arguments};
///
/// #[derive(Clone, Debug, Schema)]
/// struct Lookup { id: i64 }
///
/// let ok: Lookup = decode_arguments("lookup", &serde_json::json!({ "id": 7 })).expect("valid");
/// assert_eq!(ok.id, 7);
/// let bad = decode_arguments::<Lookup>("lookup", &serde_json::json!({ "id": "7" }));
/// assert!(matches!(bad, Err(ToolError::InvalidArguments { .. })));
/// ```
pub fn decode_arguments<I>(tool: &str, arguments: &serde_json::Value) -> Result<I, ToolError>
where
  I: HasSchema<A = I, I = Unknown>,
{
  I::schema()
    .decode_unknown_all(&unknown_from_serde_json(arguments.clone()))
    .map_err(|errors| ToolError::InvalidArguments {
      tool: tool.to_owned(),
      message: truncate_detail(errors.to_string()),
    })
}

/// Whether `name` is a valid tool name: 1 to [`MAX_TOOL_NAME_LEN`] ASCII letters, digits, `_`
/// or `-`.
pub fn is_valid_tool_name(name: &str) -> bool {
  !name.is_empty()
    && name.len() <= MAX_TOOL_NAME_LEN
    && name
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn policy_denies_writes_by_default() {
    let policy = ToolPolicy::default();
    assert_eq!(policy, ToolPolicy::read_only());
    assert!(!policy.writes_allowed());
    assert!(policy.permits(ToolEffects::Read));
    assert!(!policy.permits(ToolEffects::Write));
    let policy = policy.allow_writes();
    assert!(policy.writes_allowed());
    assert!(policy.permits(ToolEffects::Write));
    assert_eq!(ToolEffects::default(), ToolEffects::Read);
  }

  #[test]
  fn tool_names_follow_the_vendor_rule() {
    assert!(is_valid_tool_name("get_weather-v2"));
    assert!(!is_valid_tool_name(""));
    assert!(!is_valid_tool_name("has space"));
    assert!(!is_valid_tool_name("ünï"));
    assert!(is_valid_tool_name(&"a".repeat(MAX_TOOL_NAME_LEN)));
    assert!(!is_valid_tool_name(&"a".repeat(MAX_TOOL_NAME_LEN + 1)));
  }

  #[test]
  fn refusals_render_as_json_for_the_model() {
    let e = ToolError::WriteDenied {
      tool: "delete".into(),
    };
    let text: serde_json::Value = serde_json::from_str(&e.to_model_text()).expect("json");
    assert_eq!(
      text["error"],
      "tool `delete` changes state and is not allowed for this call"
    );
    assert_eq!(
      ToolError::InvalidDefinition("x".into()).to_string(),
      "invalid tool definition: x"
    );
  }
}
