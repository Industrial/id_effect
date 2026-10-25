//! [`Tool`]: a named, typed handler whose input and output have a `Schema`.

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use id_effect::schema::{json_schema_for, serde_json_from_unknown};
use id_effect::{Effect, HasSchema, Unknown};

use super::{ToolEffects, ToolError, decode_arguments};
use crate::chat::ToolDefinition;
use crate::error::truncate_detail;

type Handler<I, O> = Arc<dyn Fn(I) -> Effect<O, ToolError> + Send + Sync>;

/// A tool a model may call: a `name`, a `description` for the model, the [`ToolEffects`] it
/// declares, and a handler from the decoded input `I` to an effect producing `O`. `I` and `O`
/// implement [`HasSchema`] with a JSON-shaped wire (`#[derive(Schema)]`): `I`'s schema is the
/// tool's parameters ([`parameters_schema`](Self::parameters_schema)) and decodes every call's
/// arguments before the handler runs; `O`'s schema encodes the result.
///
/// Clones share the handler. Register tools on a [`ToolSet`](super::ToolSet), which checks the
/// name and the input shape.
pub struct Tool<I, O> {
  name: String,
  description: String,
  effects: ToolEffects,
  handler: Handler<I, O>,
  _types: PhantomData<fn(I) -> O>,
}

impl<I, O> Clone for Tool<I, O> {
  fn clone(&self) -> Self {
    Self {
      name: self.name.clone(),
      description: self.description.clone(),
      effects: self.effects,
      handler: Arc::clone(&self.handler),
      _types: PhantomData,
    }
  }
}

impl<I, O> fmt::Debug for Tool<I, O> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Tool")
      .field("name", &self.name)
      .field("description", &self.description)
      .field("effects", &self.effects)
      .finish_non_exhaustive()
  }
}

impl<I, O> Tool<I, O>
where
  I: HasSchema<A = I, I = Unknown> + Send + 'static,
  O: HasSchema<A = O, I = Unknown> + Send + 'static,
{
  /// A read-only tool `name` running `handler`. The handler's error (any `Display`) becomes
  /// [`ToolError::Failed`], which the agent loop feeds back to the model, so it should not
  /// carry secrets.
  pub fn new<E, F>(name: impl Into<String>, description: impl Into<String>, handler: F) -> Self
  where
    E: fmt::Display + Send + 'static,
    F: Fn(I) -> Effect<O, E> + Send + Sync + 'static,
  {
    let name = name.into();
    let tool = name.clone();
    let handler: Handler<I, O> = Arc::new(move |input| {
      let tool = tool.clone();
      handler(input).map_err(move |e| ToolError::Failed {
        tool,
        message: truncate_detail(e.to_string()),
      })
    });
    Self {
      name,
      description: description.into(),
      effects: ToolEffects::Read,
      handler,
      _types: PhantomData,
    }
  }

  /// Declares that this tool writes ([`ToolEffects::Write`]): it then runs only under
  /// [`ToolPolicy::allow_writes`](super::ToolPolicy::allow_writes).
  #[must_use]
  pub fn writes(mut self) -> Self {
    self.effects = ToolEffects::Write;
    self
  }

  /// The tool's name.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// The description sent to the model.
  pub fn description(&self) -> &str {
    &self.description
  }

  /// The effects the tool declares.
  pub fn effects(&self) -> ToolEffects {
    self.effects
  }

  /// The JSON Schema of the arguments: `I`'s schema exported by
  /// `id_effect::schema::json_schema_for`, without the `$schema` dialect key (vendors take the
  /// body; `$defs` stay).
  pub fn parameters_schema() -> serde_json::Value {
    without_dialect(json_schema_for::<I>())
  }

  /// The JSON Schema of the output (`O`'s schema; MCP's `outputSchema`).
  pub fn output_schema() -> serde_json::Value {
    without_dialect(json_schema_for::<O>())
  }

  /// The definition sent in [`ChatRequest::tools`](crate::ChatRequest::tools).
  pub fn definition(&self) -> ToolDefinition {
    ToolDefinition::new(
      self.name.clone(),
      self.description.clone(),
      Self::parameters_schema(),
    )
  }

  /// Runs the handler on an already decoded `input` (no policy check: [`ToolSet::invoke`]
  /// is the checked path for model calls).
  ///
  /// [`ToolSet::invoke`]: super::ToolSet::invoke
  pub fn call(&self, input: I) -> Effect<O, ToolError> {
    (self.handler)(input)
  }

  /// Decodes `arguments` through `I`'s schema, then runs the handler and encodes its output
  /// through `O`'s schema.
  pub(crate) fn call_json(
    &self,
    arguments: &serde_json::Value,
  ) -> Result<Effect<serde_json::Value, ToolError>, ToolError> {
    let input = decode_arguments::<I>(&self.name, arguments)?;
    Ok(
      self
        .call(input)
        .map(|out| serde_json_from_unknown(&O::schema().encode(out))),
    )
  }
}

/// `schema` without its top-level `$schema` key.
fn without_dialect(mut schema: serde_json::Value) -> serde_json::Value {
  if let Some(map) = schema.as_object_mut() {
    map.remove("$schema");
  }
  schema
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn without_dialect_keeps_non_objects() {
    assert_eq!(without_dialect(json!(true)), json!(true));
    assert_eq!(
      without_dialect(json!({"$schema": "x", "type": "object"})),
      json!({"type": "object"})
    );
  }
}
