//! [`ToolSet`]: the tools one call may use, and the checked path that runs a model's call.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HasSchema, Unknown, fail, succeed};

use super::tool::Tool;
use super::{ToolEffects, ToolError, ToolPolicy, is_valid_tool_name};
use crate::chat::{ToolCall, ToolDefinition};
use crate::error::truncate_detail;

/// A registered tool with its types erased.
trait ErasedTool: Send + Sync {
  fn definition(&self) -> ToolDefinition;
  fn effects(&self) -> ToolEffects;
  fn call_json(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<Effect<serde_json::Value, ToolError>, ToolError>;
}

impl<I, O> ErasedTool for Tool<I, O>
where
  I: HasSchema<A = I, I = Unknown> + Send + 'static,
  O: HasSchema<A = O, I = Unknown> + Send + 'static,
{
  fn definition(&self) -> ToolDefinition {
    Tool::definition(self)
  }

  fn effects(&self) -> ToolEffects {
    Tool::effects(self)
  }

  fn call_json(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<Effect<serde_json::Value, ToolError>, ToolError> {
    Tool::call_json(self, arguments)
  }
}

/// A tool's successful result: the output encoded through its schema, as a JSON value and as
/// the JSON text fed back to the model (at most the call's output cap).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolOutput {
  /// The output as JSON.
  pub value: serde_json::Value,
  /// `value` as compact JSON text.
  pub text: String,
}

/// The tools one model call may use, by name. Only tools registered here can run (ADR 0015 D9):
/// [`invoke`](Self::invoke) refuses every other name. Clones share the tools.
#[derive(Clone, Default)]
pub struct ToolSet {
  tools: BTreeMap<String, Arc<dyn ErasedTool>>,
}

impl fmt::Debug for ToolSet {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ToolSet")
      .field("tools", &self.tools.keys().collect::<Vec<_>>())
      .finish()
  }
}

impl ToolSet {
  /// An empty set.
  pub fn new() -> Self {
    Self::default()
  }

  /// Registers `tool`; see [`register`](Self::register).
  pub fn with<I, O>(mut self, tool: Tool<I, O>) -> Result<Self, ToolError>
  where
    I: HasSchema<A = I, I = Unknown> + Send + 'static,
    O: HasSchema<A = O, I = Unknown> + Send + 'static,
  {
    self.register(tool)?;
    Ok(self)
  }

  /// Registers `tool`. Refused with [`ToolError::InvalidDefinition`]: a name outside
  /// `[A-Za-z0-9_-]{1,64}`, a name already registered, or an input whose JSON Schema is not an
  /// `object` (both vendors require object parameters).
  pub fn register<I, O>(&mut self, tool: Tool<I, O>) -> Result<(), ToolError>
  where
    I: HasSchema<A = I, I = Unknown> + Send + 'static,
    O: HasSchema<A = O, I = Unknown> + Send + 'static,
  {
    let name = tool.name().to_owned();
    if !is_valid_tool_name(&name) {
      return Err(ToolError::InvalidDefinition(format!(
        "tool name `{}` must be 1 to 64 of [A-Za-z0-9_-]",
        truncate_detail(name)
      )));
    }
    if self.tools.contains_key(&name) {
      return Err(ToolError::InvalidDefinition(format!(
        "tool `{name}` is already registered"
      )));
    }
    if Tool::<I, O>::parameters_schema().get("type") != Some(&serde_json::json!("object")) {
      return Err(ToolError::InvalidDefinition(format!(
        "the input of tool `{name}` must be a JSON object (a struct)"
      )));
    }
    self.tools.insert(name, Arc::new(tool));
    Ok(())
  }

  /// Number of tools.
  pub fn len(&self) -> usize {
    self.tools.len()
  }

  /// Whether the set is empty.
  pub fn is_empty(&self) -> bool {
    self.tools.is_empty()
  }

  /// Whether `name` is registered.
  pub fn contains(&self, name: &str) -> bool {
    self.tools.contains_key(name)
  }

  /// The registered names, sorted.
  pub fn names(&self) -> Vec<&str> {
    self.tools.keys().map(String::as_str).collect()
  }

  /// The effects tool `name` declares (`None` when it is not registered).
  pub fn effects(&self, name: &str) -> Option<ToolEffects> {
    self.tools.get(name).map(|t| t.effects())
  }

  /// The definitions of every tool, sorted by name (for
  /// [`ChatRequest::tools`](crate::ChatRequest::tools)).
  pub fn definitions(&self) -> Vec<ToolDefinition> {
    self.tools.values().map(|t| t.definition()).collect()
  }

  /// Runs a model's `call` under the D9 rules, in order: the name must be registered
  /// ([`ToolError::Unregistered`]), the policy must permit the tool's effects
  /// ([`ToolError::WriteDenied`]), the arguments must decode through the tool's input schema
  /// ([`ToolError::InvalidArguments`]); only then does the handler run, and its output's JSON
  /// text must fit `max_output_bytes` ([`ToolError::OutputTooLarge`]).
  pub fn invoke(
    &self,
    call: &ToolCall,
    policy: &ToolPolicy,
    max_output_bytes: usize,
  ) -> Effect<ToolOutput, ToolError> {
    let Some(tool) = self.tools.get(&call.name) else {
      return fail(ToolError::Unregistered {
        name: truncate_detail(call.name.clone()),
      });
    };
    if !policy.permits(tool.effects()) {
      return fail(ToolError::WriteDenied {
        tool: call.name.clone(),
      });
    }
    let run = match tool.call_json(&call.arguments) {
      Ok(run) => run,
      Err(e) => return fail(e),
    };
    let name = call.name.clone();
    run.flat_map(move |value| {
      let text = value.to_string();
      if text.len() > max_output_bytes {
        fail(ToolError::OutputTooLarge {
          tool: name,
          max: max_output_bytes,
        })
      } else {
        succeed(ToolOutput { value, text })
      }
    })
  }
}
