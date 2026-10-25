//! The MCP values both sides exchange: implementation info, tools as listed, tool results,
//! resources, prompts and progress.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Map, Value, json};

/// The name and version of an MCP client or server (`Implementation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Implementation {
  /// Programmatic name.
  pub name: String,
  /// Version.
  pub version: String,
  /// Human-readable name.
  pub title: Option<String>,
}

impl Implementation {
  /// `name` at `version`.
  pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
    Self {
      name: name.into(),
      version: version.into(),
      title: None,
    }
  }

  /// Sets the human-readable title.
  #[must_use]
  pub fn with_title(mut self, title: impl Into<String>) -> Self {
    self.title = Some(title.into());
    self
  }

  /// As JSON.
  pub fn to_json(&self) -> Value {
    let mut obj = Map::new();
    obj.insert("name".into(), Value::String(self.name.clone()));
    obj.insert("version".into(), Value::String(self.version.clone()));
    if let Some(title) = &self.title {
      obj.insert("title".into(), Value::String(title.clone()));
    }
    Value::Object(obj)
  }

  /// From JSON (`None` without a string `name` and `version`).
  pub fn from_json(value: &Value) -> Option<Self> {
    Some(Self {
      name: value.get("name")?.as_str()?.to_owned(),
      version: value.get("version")?.as_str()?.to_owned(),
      title: opt_str(value, "title"),
    })
  }
}

/// What `server/discover` answered: the revisions, capabilities and identity of a server.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ServerDescription {
  /// The protocol revisions the server implements.
  pub supported_versions: Vec<String>,
  /// The server's capabilities object (`tools`, `resources`, `prompts`, ...).
  pub capabilities: Value,
  /// The server's identity, when it sent one.
  pub server_info: Option<Implementation>,
  /// Guidance for using the server.
  pub instructions: Option<String>,
}

impl ServerDescription {
  /// Whether the server offers capability `name` (`"tools"`, `"resources"`, `"prompts"`).
  pub fn has_capability(&self, name: &str) -> bool {
    self.capabilities.get(name).is_some_and(|v| !v.is_null())
  }

  pub(crate) fn from_json(value: &Value) -> Self {
    Self {
      supported_versions: value
        .get("supportedVersions")
        .and_then(Value::as_array)
        .map(|vs| {
          vs.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
        })
        .unwrap_or_default(),
      capabilities: value.get("capabilities").cloned().unwrap_or(json!({})),
      server_info: value
        .get("_meta")
        .and_then(|m| m.get(super::SERVER_INFO_META_KEY))
        .and_then(Implementation::from_json),
      instructions: opt_str(value, "instructions"),
    }
  }
}

/// A tool as a server lists it (`tools/list`).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct McpTool {
  /// The name `tools/call` takes.
  pub name: String,
  /// Human-readable name.
  pub title: Option<String>,
  /// What the tool does.
  pub description: Option<String>,
  /// JSON Schema (an object) of the arguments.
  pub input_schema: Value,
  /// Behaviour hints (`readOnlyHint`, `destructiveHint`, ...), untrusted.
  pub annotations: Option<Value>,
}

impl McpTool {
  /// Whether the server marks the tool read-only (an untrusted hint).
  pub fn read_only_hint(&self) -> bool {
    self
      .annotations
      .as_ref()
      .and_then(|a| a.get("readOnlyHint"))
      .and_then(Value::as_bool)
      .unwrap_or(false)
  }

  pub(crate) fn from_json(value: &Value) -> Option<Self> {
    Some(Self {
      name: value.get("name")?.as_str()?.to_owned(),
      title: opt_str(value, "title"),
      description: opt_str(value, "description"),
      input_schema: value.get("inputSchema").filter(|s| s.is_object())?.clone(),
      annotations: value.get("annotations").filter(|a| a.is_object()).cloned(),
    })
  }
}

/// One content block of a tool result or prompt message.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Content {
  /// `{"type": "text", "text": ...}`.
  Text(String),
  /// Any other block (image, audio, resource link, embedded resource), as sent.
  Other(Value),
}

impl Content {
  /// A text block.
  pub fn text(text: impl Into<String>) -> Self {
    Self::Text(text.into())
  }

  /// The text of a text block.
  pub fn as_text(&self) -> Option<&str> {
    match self {
      Self::Text(t) => Some(t),
      Self::Other(_) => None,
    }
  }

  /// As JSON.
  pub fn to_json(&self) -> Value {
    match self {
      Self::Text(t) => json!({ "type": "text", "text": t }),
      Self::Other(v) => v.clone(),
    }
  }

  /// From JSON.
  pub fn from_json(value: &Value) -> Self {
    match (
      value.get("type").and_then(Value::as_str),
      value.get("text").and_then(Value::as_str),
    ) {
      (Some("text"), Some(text)) => Self::Text(text.to_owned()),
      _ => Self::Other(value.clone()),
    }
  }
}

/// The result of `tools/call`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ToolResult {
  /// The content blocks.
  pub content: Vec<Content>,
  /// The structured result, when the tool returned an object.
  pub structured_content: Option<Value>,
  /// Whether the tool failed (its content then says why). A tool failure is a result, not a
  /// protocol error, so a model can read it.
  pub is_error: bool,
}

impl ToolResult {
  /// The text blocks joined with newlines.
  pub fn text(&self) -> String {
    self
      .content
      .iter()
      .filter_map(Content::as_text)
      .collect::<Vec<_>>()
      .join("\n")
  }

  pub(crate) fn to_json(&self) -> Value {
    let mut obj = Map::new();
    obj.insert("resultType".into(), json!("complete"));
    obj.insert(
      "content".into(),
      Value::Array(self.content.iter().map(Content::to_json).collect()),
    );
    if let Some(s) = &self.structured_content {
      obj.insert("structuredContent".into(), s.clone());
    }
    obj.insert("isError".into(), Value::Bool(self.is_error));
    Value::Object(obj)
  }

  pub(crate) fn from_json(value: &Value) -> Self {
    Self {
      content: value
        .get("content")
        .and_then(Value::as_array)
        .map(|c| c.iter().map(Content::from_json).collect())
        .unwrap_or_default(),
      structured_content: value.get("structuredContent").cloned(),
      is_error: value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false),
    }
  }
}

/// A resource a server offers (`resources/list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
  /// Its URI (what `resources/read` takes).
  pub uri: String,
  /// Programmatic name.
  pub name: String,
  /// Human-readable name.
  pub title: Option<String>,
  /// What it holds.
  pub description: Option<String>,
  /// Its media type.
  pub mime_type: Option<String>,
}

impl Resource {
  /// Resource `name` at `uri`.
  pub fn new(uri: impl Into<String>, name: impl Into<String>) -> Self {
    Self {
      uri: uri.into(),
      name: name.into(),
      title: None,
      description: None,
      mime_type: None,
    }
  }

  /// Sets the title.
  #[must_use]
  pub fn with_title(mut self, title: impl Into<String>) -> Self {
    self.title = Some(title.into());
    self
  }

  /// Sets the description.
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }

  /// Sets the media type.
  #[must_use]
  pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
    self.mime_type = Some(mime_type.into());
    self
  }

  pub(crate) fn to_json(&self) -> Value {
    let mut obj = Map::new();
    obj.insert("uri".into(), Value::String(self.uri.clone()));
    obj.insert("name".into(), Value::String(self.name.clone()));
    put_opt(&mut obj, "title", &self.title);
    put_opt(&mut obj, "description", &self.description);
    put_opt(&mut obj, "mimeType", &self.mime_type);
    Value::Object(obj)
  }

  pub(crate) fn from_json(value: &Value) -> Option<Self> {
    Some(Self {
      uri: value.get("uri")?.as_str()?.to_owned(),
      name: value.get("name")?.as_str()?.to_owned(),
      title: opt_str(value, "title"),
      description: opt_str(value, "description"),
      mime_type: opt_str(value, "mimeType"),
    })
  }
}

/// What a resource reader returns: text or bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceBody {
  /// UTF-8 text.
  Text(String),
  /// Binary data (sent base64-encoded).
  Blob(Vec<u8>),
}

/// The contents of a resource as `resources/read` returns them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResourceContents {
  /// The URI read.
  pub uri: String,
  /// The media type.
  pub mime_type: Option<String>,
  /// The data.
  pub body: ResourceBody,
}

impl ResourceContents {
  /// The text, for a text resource.
  pub fn text(&self) -> Option<&str> {
    match &self.body {
      ResourceBody::Text(t) => Some(t),
      ResourceBody::Blob(_) => None,
    }
  }

  pub(crate) fn to_json(&self) -> Value {
    let mut obj = Map::new();
    obj.insert("uri".into(), Value::String(self.uri.clone()));
    put_opt(&mut obj, "mimeType", &self.mime_type);
    match &self.body {
      ResourceBody::Text(t) => obj.insert("text".into(), Value::String(t.clone())),
      ResourceBody::Blob(b) => obj.insert("blob".into(), Value::String(BASE64.encode(b))),
    };
    Value::Object(obj)
  }

  pub(crate) fn from_json(value: &Value) -> Option<Self> {
    let body = match (
      value.get("text").and_then(Value::as_str),
      value.get("blob").and_then(Value::as_str),
    ) {
      (Some(t), _) => ResourceBody::Text(t.to_owned()),
      (None, Some(b)) => ResourceBody::Blob(BASE64.decode(b).ok()?),
      (None, None) => return None,
    };
    Some(Self {
      uri: value.get("uri")?.as_str()?.to_owned(),
      mime_type: opt_str(value, "mimeType"),
      body,
    })
  }
}

/// One argument a prompt template takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptArgument {
  /// Its name.
  pub name: String,
  /// What it is for.
  pub description: Option<String>,
  /// Whether `prompts/get` must supply it.
  pub required: bool,
}

impl PromptArgument {
  /// A required argument.
  pub fn required(name: impl Into<String>, description: impl Into<String>) -> Self {
    Self {
      name: name.into(),
      description: Some(description.into()),
      required: true,
    }
  }

  /// An optional argument.
  pub fn optional(name: impl Into<String>, description: impl Into<String>) -> Self {
    Self {
      required: false,
      ..Self::required(name, description)
    }
  }
}

/// A prompt template a server offers (`prompts/list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
  /// Its name (what `prompts/get` takes).
  pub name: String,
  /// Human-readable name.
  pub title: Option<String>,
  /// What it is for.
  pub description: Option<String>,
  /// The arguments it takes.
  pub arguments: Vec<PromptArgument>,
}

impl Prompt {
  /// Prompt `name` without arguments.
  pub fn new(name: impl Into<String>) -> Self {
    Self {
      name: name.into(),
      title: None,
      description: None,
      arguments: Vec::new(),
    }
  }

  /// Sets the description.
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }

  /// Sets the title.
  #[must_use]
  pub fn with_title(mut self, title: impl Into<String>) -> Self {
    self.title = Some(title.into());
    self
  }

  /// Adds an argument.
  #[must_use]
  pub fn with_argument(mut self, argument: PromptArgument) -> Self {
    self.arguments.push(argument);
    self
  }

  pub(crate) fn to_json(&self) -> Value {
    let mut obj = Map::new();
    obj.insert("name".into(), Value::String(self.name.clone()));
    put_opt(&mut obj, "title", &self.title);
    put_opt(&mut obj, "description", &self.description);
    let args = self
      .arguments
      .iter()
      .map(|a| {
        let mut arg = Map::new();
        arg.insert("name".into(), Value::String(a.name.clone()));
        put_opt(&mut arg, "description", &a.description);
        arg.insert("required".into(), Value::Bool(a.required));
        Value::Object(arg)
      })
      .collect();
    obj.insert("arguments".into(), Value::Array(args));
    Value::Object(obj)
  }

  pub(crate) fn from_json(value: &Value) -> Option<Self> {
    let arguments = value
      .get("arguments")
      .and_then(Value::as_array)
      .map(|args| {
        args
          .iter()
          .filter_map(|a| {
            Some(PromptArgument {
              name: a.get("name")?.as_str()?.to_owned(),
              description: opt_str(a, "description"),
              required: a.get("required").and_then(Value::as_bool).unwrap_or(false),
            })
          })
          .collect()
      })
      .unwrap_or_default();
    Some(Self {
      name: value.get("name")?.as_str()?.to_owned(),
      title: opt_str(value, "title"),
      description: opt_str(value, "description"),
      arguments,
    })
  }
}

/// Who speaks a prompt message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRole {
  /// The user.
  User,
  /// The assistant.
  Assistant,
}

/// One message of a rendered prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptMessage {
  /// Who speaks.
  pub role: PromptRole,
  /// What is said.
  pub content: Content,
}

impl PromptMessage {
  /// A user text message.
  pub fn user(text: impl Into<String>) -> Self {
    Self {
      role: PromptRole::User,
      content: Content::text(text),
    }
  }

  /// An assistant text message.
  pub fn assistant(text: impl Into<String>) -> Self {
    Self {
      role: PromptRole::Assistant,
      content: Content::text(text),
    }
  }

  pub(crate) fn to_json(&self) -> Value {
    let role = match self.role {
      PromptRole::User => "user",
      PromptRole::Assistant => "assistant",
    };
    json!({ "role": role, "content": self.content.to_json() })
  }

  pub(crate) fn from_json(value: &Value) -> Option<Self> {
    let role = match value.get("role")?.as_str()? {
      "user" => PromptRole::User,
      "assistant" => PromptRole::Assistant,
      _ => return None,
    };
    Some(Self {
      role,
      content: Content::from_json(value.get("content")?),
    })
  }
}

/// A rendered prompt (`prompts/get`).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PromptReply {
  /// The prompt's description.
  pub description: Option<String>,
  /// The messages.
  pub messages: Vec<PromptMessage>,
}

impl PromptReply {
  pub(crate) fn from_json(value: &Value) -> Self {
    Self {
      description: opt_str(value, "description"),
      messages: value
        .get("messages")
        .and_then(Value::as_array)
        .map(|m| m.iter().filter_map(PromptMessage::from_json).collect())
        .unwrap_or_default(),
    }
  }
}

/// One progress notification of a running request.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Progress {
  /// Progress so far (increases with every notification).
  pub progress: f64,
  /// The total, when known.
  pub total: Option<f64>,
  /// A human-readable status.
  pub message: Option<String>,
}

impl Progress {
  pub(crate) fn from_json(value: &Value) -> Option<Self> {
    Some(Self {
      progress: value.get("progress")?.as_f64()?,
      total: value.get("total").and_then(Value::as_f64),
      message: opt_str(value, "message"),
    })
  }
}

pub(crate) fn opt_str(value: &Value, key: &str) -> Option<String> {
  value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn put_opt(obj: &mut Map<String, Value>, key: &str, value: &Option<String>) {
  if let Some(v) = value {
    obj.insert(key.into(), Value::String(v.clone()));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn values_round_trip_through_json() {
    let imp = Implementation::new("srv", "1.0").with_title("Server");
    assert_eq!(Implementation::from_json(&imp.to_json()), Some(imp));
    assert_eq!(Implementation::from_json(&json!({ "name": "x" })), None);

    let r = Resource::new("doc://1", "one")
      .with_title("One")
      .with_description("first")
      .with_mime_type("text/plain");
    assert_eq!(Resource::from_json(&r.to_json()), Some(r));
    assert_eq!(Resource::from_json(&json!({ "uri": "x" })), None);

    let text = ResourceContents {
      uri: "doc://1".into(),
      mime_type: Some("text/plain".into()),
      body: ResourceBody::Text("hi".into()),
    };
    assert_eq!(
      ResourceContents::from_json(&text.to_json()),
      Some(text.clone())
    );
    assert_eq!(text.text(), Some("hi"));
    let blob = ResourceContents {
      uri: "bin://1".into(),
      mime_type: None,
      body: ResourceBody::Blob(vec![0, 1, 2]),
    };
    assert_eq!(
      ResourceContents::from_json(&blob.to_json()),
      Some(blob.clone())
    );
    assert_eq!(blob.text(), None);
    assert_eq!(ResourceContents::from_json(&json!({ "uri": "x" })), None);
    assert_eq!(
      ResourceContents::from_json(&json!({ "uri": "x", "blob": "%%" })),
      None
    );

    let p = Prompt::new("summarize")
      .with_title("Summarize")
      .with_description("Summarize a document")
      .with_argument(PromptArgument::required("doc", "the document"))
      .with_argument(PromptArgument::optional("style", "tone"));
    assert_eq!(Prompt::from_json(&p.to_json()), Some(p));
    assert_eq!(Prompt::from_json(&json!({})), None);

    for m in [PromptMessage::user("u"), PromptMessage::assistant("a")] {
      assert_eq!(PromptMessage::from_json(&m.to_json()), Some(m));
    }
    assert_eq!(
      PromptMessage::from_json(&json!({ "role": "system", "content": {} })),
      None
    );
    let reply = PromptReply::from_json(&json!({
      "description": "d",
      "messages": [{ "role": "user", "content": { "type": "text", "text": "x" } }],
    }));
    assert_eq!(reply.description.as_deref(), Some("d"));
    assert_eq!(reply.messages.len(), 1);
  }

  #[test]
  fn tool_results_content_and_listings() {
    let result = ToolResult {
      content: vec![
        Content::text("a"),
        Content::Other(json!({ "type": "image" })),
        Content::text("b"),
      ],
      structured_content: Some(json!({ "x": 1 })),
      is_error: false,
    };
    let back = ToolResult::from_json(&result.to_json());
    assert_eq!(back, result);
    assert_eq!(back.text(), "a\nb");
    assert_eq!(Content::Other(json!(1)).as_text(), None);
    assert_eq!(ToolResult::from_json(&json!({})).content, vec![]);

    let tool = McpTool::from_json(&json!({
      "name": "t", "title": "T", "description": "d",
      "inputSchema": { "type": "object" },
      "annotations": { "readOnlyHint": true },
    }))
    .unwrap();
    assert!(tool.read_only_hint());
    let bare =
      McpTool::from_json(&json!({ "name": "t", "inputSchema": { "type": "object" } })).unwrap();
    assert!(!bare.read_only_hint());
    assert!(McpTool::from_json(&json!({ "name": "t", "inputSchema": 1 })).is_none());

    let desc = ServerDescription::from_json(&json!({
      "supportedVersions": ["2026-07-28"],
      "capabilities": { "tools": {} },
      "instructions": "hi",
      "_meta": { "io.modelcontextprotocol/serverInfo": { "name": "s", "version": "1" } },
    }));
    assert!(desc.has_capability("tools"));
    assert!(!desc.has_capability("prompts"));
    assert_eq!(desc.server_info.unwrap().name, "s");
    let empty = ServerDescription::from_json(&json!({}));
    assert!(empty.supported_versions.is_empty());

    let p = Progress::from_json(&json!({ "progress": 1, "total": 2, "message": "m" })).unwrap();
    assert_eq!(p.total, Some(2.0));
    assert!(Progress::from_json(&json!({})).is_none());
  }
}
