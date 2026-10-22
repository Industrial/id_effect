//! [`McpServer`]: tools (from a [`ToolSet`]), resources and prompts, answered per the pinned
//! revision; the transports ([`McpServer::serve_stdio`], the Streamable HTTP endpoint) run each
//! request as one fiber through [`McpServer::start`].

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use id_effect::observability::posture::record_dangerous;
use id_effect::{
  Cause, Effect, Exit, FiberContext, JoinFuture, Runtime, TokioExecutor, fail, succeed,
};
use serde_json::{Map, Value, json};

use super::context::{Handles, ProgressSink, RequestScope, within};
use super::message::{
  DEFAULT_MAX_MESSAGE_BYTES, INTERNAL_ERROR, Message, Request, RequestId, RpcError, SERVER_BUSY,
  UNSUPPORTED_PROTOCOL_VERSION,
};
use super::types::{
  Implementation, Prompt, PromptMessage, Resource, ResourceBody, ResourceContents, ToolResult,
};
use super::{
  META_CLIENT_CAPABILITIES, META_PROTOCOL_VERSION, PROTOCOL_VERSION, SERVER_INFO_META_KEY,
};
use crate::chat::ToolCall;
use crate::error::truncate_detail;
use crate::tools::{DEFAULT_MAX_TOOL_OUTPUT_BYTES, ToolEffects, ToolError, ToolPolicy, ToolSet};

/// Posture id of [`McpServer::dangerous_mcp_write_tools`] (what the prod posture check reads).
pub const DANGEROUS_MCP_WRITE_TOOLS: &str =
  "id_effect_ai::mcp::McpServer::dangerous_mcp_write_tools";

const REOPENS_MCP_WRITE_TOOLS: &str = "R10: tools that change state are callable by every MCP \
  client the endpoint admits (a model's choice of tool is untrusted input)";

/// Finite limits of an MCP server (ADR 0013 R4). None is infinite; the setters keep each at
/// least 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct McpLimits {
  /// Cap on one message, request or response (default 1 MiB).
  pub max_message_bytes: usize,
  /// Requests in flight at once per stdio connection, or per principal over HTTP (default 16).
  pub max_in_flight: usize,
  /// Cap on one tool's output (default 64 KiB, ADR 0015 D9).
  pub max_tool_output_bytes: usize,
  /// Items per page of `tools/list`, `resources/list` and `prompts/list` (default 100).
  pub page_size: usize,
  /// Handles and cursors one principal holds; the oldest is evicted (default 256).
  pub max_handles_per_principal: usize,
  /// Handles and cursors the server holds in all (default 8192).
  pub max_handles: usize,
  /// How long a handle or cursor stays valid (default 1 h).
  pub handle_ttl: Duration,
}

impl McpLimits {
  /// Replaces [`max_message_bytes`](Self::max_message_bytes).
  #[must_use]
  pub fn with_max_message_bytes(mut self, n: usize) -> Self {
    self.max_message_bytes = n.max(1);
    self
  }

  /// Replaces [`max_in_flight`](Self::max_in_flight).
  #[must_use]
  pub fn with_max_in_flight(mut self, n: usize) -> Self {
    self.max_in_flight = n.max(1);
    self
  }

  /// Replaces [`max_tool_output_bytes`](Self::max_tool_output_bytes).
  #[must_use]
  pub fn with_max_tool_output_bytes(mut self, n: usize) -> Self {
    self.max_tool_output_bytes = n.max(1);
    self
  }

  /// Replaces [`page_size`](Self::page_size).
  #[must_use]
  pub fn with_page_size(mut self, n: usize) -> Self {
    self.page_size = n.max(1);
    self
  }

  /// Replaces [`max_handles_per_principal`](Self::max_handles_per_principal) and
  /// [`max_handles`](Self::max_handles).
  #[must_use]
  pub fn with_max_handles(mut self, per_principal: usize, total: usize) -> Self {
    self.max_handles_per_principal = per_principal.max(1);
    self.max_handles = total.max(1);
    self
  }

  /// Replaces [`handle_ttl`](Self::handle_ttl) (at least 1 s).
  #[must_use]
  pub fn with_handle_ttl(mut self, ttl: Duration) -> Self {
    self.handle_ttl = ttl.max(Duration::from_secs(1));
    self
  }
}

impl Default for McpLimits {
  fn default() -> Self {
    Self {
      max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
      max_in_flight: 16,
      max_tool_output_bytes: DEFAULT_MAX_TOOL_OUTPUT_BYTES,
      page_size: 100,
      max_handles_per_principal: 256,
      max_handles: 8192,
      handle_ttl: Duration::from_secs(3600),
    }
  }
}

type ResourceReader = Arc<dyn Fn() -> Effect<ResourceBody, String> + Send + Sync>;
type PromptRenderer =
  Arc<dyn Fn(BTreeMap<String, String>) -> Effect<Vec<PromptMessage>, String> + Send + Sync>;

/// An MCP server: tools, resources and prompts (see the [module docs](super)).
///
/// Tools are the same [`Tool`](crate::tools::Tool) values the agent loop runs, registered on a
/// [`ToolSet`], and every call goes through [`ToolSet::invoke`] (registered names only, arguments
/// decoded through the tool's `Schema`, output capped). Write tools are not listed and do not
/// run unless [`dangerous_mcp_write_tools`](Self::dangerous_mcp_write_tools) is taken.
///
/// ```
/// use id_effect::{Effect, Schema, succeed};
/// use id_effect_ai::mcp::{McpServer, Prompt, PromptMessage, Resource, ResourceBody};
/// use id_effect_ai::tools::{Tool, ToolSet};
///
/// #[derive(Clone, Debug, Schema)]
/// struct Add { a: i64, b: i64 }
/// #[derive(Clone, Debug, Schema)]
/// struct Sum { sum: i64 }
///
/// let tools = ToolSet::new()
///   .with(Tool::new("add", "Adds two integers", |i: Add| -> Effect<Sum, String> {
///     succeed(Sum { sum: i.a + i.b })
///   }))
///   .expect("valid tool");
/// let server = McpServer::new("calc", "1.0.0")
///   .with_tools(tools)
///   .with_resource(Resource::new("calc://readme", "readme"), || {
///     succeed::<_, String, _>(ResourceBody::Text("adds numbers".into()))
///   })
///   .with_prompt(Prompt::new("explain"), |_args| {
///     succeed::<_, String, _>(vec![PromptMessage::user("Explain addition")])
///   });
/// assert!(!server.writes_allowed());
/// ```
#[derive(Clone)]
pub struct McpServer {
  info: Implementation,
  instructions: Option<String>,
  tools: ToolSet,
  policy: ToolPolicy,
  resources: BTreeMap<String, (Resource, ResourceReader)>,
  prompts: BTreeMap<String, (Prompt, PromptRenderer)>,
  limits: McpLimits,
  handles: Handles,
}

impl fmt::Debug for McpServer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("McpServer")
      .field("info", &self.info)
      .field("tools", &self.tools)
      .field("writes_allowed", &self.policy.writes_allowed())
      .field("resources", &self.resources.keys().collect::<Vec<_>>())
      .field("prompts", &self.prompts.keys().collect::<Vec<_>>())
      .field("limits", &self.limits)
      .finish()
  }
}

fn handles_for(limits: &McpLimits) -> Handles {
  Handles::new(
    limits.max_handles_per_principal,
    limits.max_handles,
    limits.handle_ttl,
  )
}

impl McpServer {
  /// A server named `name` at `version`, with nothing registered and the default
  /// [`McpLimits`].
  pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
    let limits = McpLimits::default();
    Self {
      info: Implementation::new(name, version),
      instructions: None,
      tools: ToolSet::new(),
      policy: ToolPolicy::read_only(),
      resources: BTreeMap::new(),
      prompts: BTreeMap::new(),
      handles: handles_for(&limits),
      limits,
    }
  }

  /// Replaces the server's identity.
  #[must_use]
  pub fn with_info(mut self, info: Implementation) -> Self {
    self.info = info;
    self
  }

  /// Guidance for clients, sent with `server/discover`.
  #[must_use]
  pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
    self.instructions = Some(instructions.into());
    self
  }

  /// Serves `tools` (replacing any set given before).
  #[must_use]
  pub fn with_tools(mut self, tools: ToolSet) -> Self {
    self.tools = tools;
    self
  }

  /// Lists and runs the tools declared [`ToolEffects::Write`] too: every client the transport
  /// admits may then change state through them. Off by default (ADR 0015 D9, ADR 0013 R1);
  /// records [`DANGEROUS_MCP_WRITE_TOOLS`] in the posture registry, so the prod posture check
  /// refuses to start unless the operator accepted it.
  #[must_use]
  pub fn dangerous_mcp_write_tools(mut self) -> Self {
    record_dangerous(DANGEROUS_MCP_WRITE_TOOLS, REOPENS_MCP_WRITE_TOOLS);
    self.policy = ToolPolicy::read_only().allow_writes();
    self
  }

  /// Whether [`dangerous_mcp_write_tools`](Self::dangerous_mcp_write_tools) was taken.
  pub fn writes_allowed(&self) -> bool {
    self.policy.writes_allowed()
  }

  /// Offers `resource`, read by `reader` on every `resources/read` (a later resource with the
  /// same URI replaces it). The reader's error text is sent to the client, so it should not
  /// carry secrets.
  #[must_use]
  pub fn with_resource<E, F>(mut self, resource: Resource, reader: F) -> Self
  where
    E: fmt::Display + Send + 'static,
    F: Fn() -> Effect<ResourceBody, E> + Send + Sync + 'static,
  {
    let reader: ResourceReader = Arc::new(move || reader().map_err(|e| e.to_string()));
    self
      .resources
      .insert(resource.uri.clone(), (resource, reader));
    self
  }

  /// Offers `prompt`, rendered by `render` from the client's arguments (required arguments
  /// are checked first; a later prompt with the same name replaces it).
  #[must_use]
  pub fn with_prompt<E, F>(mut self, prompt: Prompt, render: F) -> Self
  where
    E: fmt::Display + Send + 'static,
    F: Fn(BTreeMap<String, String>) -> Effect<Vec<PromptMessage>, E> + Send + Sync + 'static,
  {
    let render: PromptRenderer = Arc::new(move |args| render(args).map_err(|e| e.to_string()));
    self.prompts.insert(prompt.name.clone(), (prompt, render));
    self
  }

  /// Replaces the [`McpLimits`] (and starts a new handle store under them).
  #[must_use]
  pub fn with_limits(mut self, limits: McpLimits) -> Self {
    self.handles = handles_for(&limits);
    self.limits = limits;
    self
  }

  /// The server's identity.
  pub fn info(&self) -> &Implementation {
    &self.info
  }

  /// The limits.
  pub fn limits(&self) -> &McpLimits {
    &self.limits
  }

  /// The tools served.
  pub fn tools(&self) -> &ToolSet {
    &self.tools
  }

  /// Starts answering `request` for `principal` as one fiber on the current tokio runtime (or
  /// the global runtime). Progress notifications go to `progress` when the request carries a
  /// `progressToken`. Dropping the returned future before it is done interrupts the fiber (its
  /// finalizers run): that is how a transport cancels a request.
  pub(crate) fn start(
    &self,
    request: Request,
    principal: Arc<str>,
    progress: Option<ProgressSink>,
  ) -> RunningRequest {
    let id = request.id.clone();
    let eff = self.answer(request, principal, progress);
    let runtime = match TokioExecutor::current() {
      Some(exec) => Runtime::from_executor(Arc::new(exec)),
      None => Runtime::global().clone(),
    };
    let handle = runtime.fork(eff);
    RunningRequest {
      id,
      fiber: Arc::clone(handle.fiber()),
      join: Some(handle.join_future()),
    }
  }

  /// The effect answering one request: the protocol checks, then the method.
  pub(crate) fn answer(
    &self,
    request: Request,
    principal: Arc<str>,
    progress: Option<ProgressSink>,
  ) -> Effect<Value, RpcError> {
    let params = request.params.unwrap_or_else(|| json!({}));
    if let Err(e) = check_meta(&params) {
      return fail(e);
    }
    let token = params
      .get("_meta")
      .and_then(|m| m.get("progressToken"))
      .filter(|t| t.is_string() || t.is_i64())
      .cloned();
    let scope = RequestScope {
      principal: Arc::clone(&principal),
      progress: token.zip(progress),
      handles: self.handles.clone(),
    };
    let eff = match request.method.as_str() {
      "server/discover" => succeed(self.discover()),
      "tools/list" => done(self.page("tools", self.tool_listing(), &params, &principal)),
      "tools/call" => self.call_tool(&request.id, &params),
      "resources/list" => {
        let items = self.resources.values().map(|(r, _)| r.to_json()).collect();
        done(self.page("resources", items, &params, &principal))
      }
      "resources/templates/list" => {
        done(self.page("resourceTemplates", Vec::new(), &params, &principal))
      }
      "resources/read" => self.read_resource(&params),
      "prompts/list" => {
        let items = self.prompts.values().map(|(p, _)| p.to_json()).collect();
        done(self.page("prompts", items, &params, &principal))
      }
      "prompts/get" => self.get_prompt(&params),
      other => fail(RpcError::method_not_found(other)),
    };
    within(scope, eff)
  }

  fn discover(&self) -> Value {
    let mut caps = Map::new();
    if !self.tools.is_empty() {
      caps.insert("tools".into(), json!({}));
    }
    if !self.resources.is_empty() {
      caps.insert("resources".into(), json!({}));
    }
    if !self.prompts.is_empty() {
      caps.insert("prompts".into(), json!({}));
    }
    let mut obj = Map::new();
    obj.insert("resultType".into(), json!("complete"));
    obj.insert("supportedVersions".into(), json!([PROTOCOL_VERSION]));
    obj.insert("capabilities".into(), Value::Object(caps));
    if let Some(i) = &self.instructions {
      obj.insert("instructions".into(), Value::String(i.clone()));
    }
    obj.insert("ttlMs".into(), json!(0));
    obj.insert("cacheScope".into(), json!("private"));
    obj.insert(
      "_meta".into(),
      json!({ SERVER_INFO_META_KEY: self.info.to_json() }),
    );
    Value::Object(obj)
  }

  /// The tools a client sees: write tools only under the write opt-out.
  fn tool_listing(&self) -> Vec<Value> {
    self
      .tools
      .definitions()
      .into_iter()
      .filter_map(|d| {
        let effects = self.tools.effects(&d.name)?;
        if !self.policy.permits(effects) {
          return None;
        }
        let read_only = effects == ToolEffects::Read;
        Some(json!({
          "name": d.name,
          "description": d.description,
          "inputSchema": d.parameters,
          "annotations": { "readOnlyHint": read_only, "destructiveHint": !read_only },
        }))
      })
      .collect()
  }

  /// One page of `items` under `key`, from the principal's cursor; the next cursor is a handle
  /// bound to the principal.
  fn page(
    &self,
    key: &str,
    items: Vec<Value>,
    params: &Value,
    principal: &Arc<str>,
  ) -> Result<Value, RpcError> {
    let bad_cursor = || RpcError::invalid_params("invalid cursor");
    let offset = match params.get("cursor") {
      None | Some(Value::Null) => 0,
      Some(Value::String(c)) => {
        let state = self
          .handles
          .resolve(principal, c)
          .map_err(|_| bad_cursor())?;
        if state.get("list").and_then(Value::as_str) != Some(key) {
          return Err(bad_cursor());
        }
        state
          .get("offset")
          .and_then(Value::as_u64)
          .map_or(0, |o| usize::try_from(o).unwrap_or(usize::MAX))
      }
      Some(_) => return Err(bad_cursor()),
    };
    let end = offset
      .saturating_add(self.limits.page_size)
      .min(items.len());
    let page: Vec<Value> = items
      .get(offset.min(items.len())..end)
      .unwrap_or_default()
      .to_vec();
    let mut obj = Map::new();
    obj.insert("resultType".into(), json!("complete"));
    obj.insert(key.into(), Value::Array(page));
    obj.insert("ttlMs".into(), json!(0));
    obj.insert("cacheScope".into(), json!("private"));
    if end < items.len() {
      let next = self
        .handles
        .mint(principal, json!({ "list": key, "offset": end }))
        .map_err(|e| RpcError::new(SERVER_BUSY, e.to_string()))?;
      obj.insert("nextCursor".into(), Value::String(next));
    }
    Ok(Value::Object(obj))
  }

  fn call_tool(&self, id: &RequestId, params: &Value) -> Effect<Value, RpcError> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
      return fail(RpcError::invalid_params("`name` is required"));
    };
    let arguments = match params.get("arguments") {
      None | Some(Value::Null) => json!({}),
      Some(a @ Value::Object(_)) => a.clone(),
      Some(_) => return fail(RpcError::invalid_params("`arguments` must be an object")),
    };
    if !self.tools.contains(name) {
      return fail(
        RpcError::invalid_params(format!(
          "Unknown tool: {}",
          truncate_detail(name.to_owned())
        ))
        .with_data(json!({ "tool": truncate_detail(name.to_owned()) })),
      );
    }
    let call = ToolCall::new(id.to_string(), name, arguments);
    self
      .tools
      .invoke(&call, &self.policy, self.limits.max_tool_output_bytes)
      .map(|out| {
        let structured = out.value.is_object().then_some(out.value);
        ToolResult {
          content: vec![super::types::Content::text(out.text)],
          structured_content: structured,
          is_error: false,
        }
        .to_json()
      })
      .catch(|e: ToolError| {
        succeed(
          ToolResult {
            content: vec![super::types::Content::text(e.to_string())],
            structured_content: None,
            is_error: true,
          }
          .to_json(),
        )
      })
  }

  fn read_resource(&self, params: &Value) -> Effect<Value, RpcError> {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
      return fail(RpcError::invalid_params("`uri` is required"));
    };
    let Some((resource, reader)) = self.resources.get(uri) else {
      return fail(
        RpcError::invalid_params("Resource not found")
          .with_data(json!({ "uri": truncate_detail(uri.to_owned()) })),
      );
    };
    let uri = resource.uri.clone();
    let mime_type = resource.mime_type.clone();
    let max = self.limits.max_message_bytes;
    reader()
      .map_err(|e| {
        RpcError::new(
          INTERNAL_ERROR,
          truncate_detail(format!("resource read failed: {e}")),
        )
      })
      .flat_map(move |body| {
        let len = match &body {
          ResourceBody::Text(t) => t.len(),
          ResourceBody::Blob(b) => b.len().saturating_mul(4) / 3,
        };
        if len > max {
          return fail(RpcError::new(
            INTERNAL_ERROR,
            format!("resource exceeds {max} bytes"),
          ));
        }
        let contents = ResourceContents {
          uri,
          mime_type,
          body,
        };
        succeed(json!({ "resultType": "complete", "contents": [contents.to_json()] }))
      })
  }

  fn get_prompt(&self, params: &Value) -> Effect<Value, RpcError> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
      return fail(RpcError::invalid_params("`name` is required"));
    };
    let Some((prompt, render)) = self.prompts.get(name) else {
      return fail(
        RpcError::invalid_params("Unknown prompt")
          .with_data(json!({ "prompt": truncate_detail(name.to_owned()) })),
      );
    };
    let mut args = BTreeMap::new();
    match params.get("arguments") {
      None | Some(Value::Null) => {}
      Some(Value::Object(obj)) => {
        for (k, v) in obj {
          let Value::String(v) = v else {
            return fail(RpcError::invalid_params(format!(
              "argument `{}` must be a string",
              truncate_detail(k.clone())
            )));
          };
          args.insert(k.clone(), v.clone());
        }
      }
      Some(_) => return fail(RpcError::invalid_params("`arguments` must be an object")),
    }
    if let Some(missing) = prompt
      .arguments
      .iter()
      .find(|a| a.required && !args.contains_key(&a.name))
    {
      return fail(RpcError::invalid_params(format!(
        "missing required argument `{}`",
        missing.name
      )));
    }
    let description = prompt.description.clone();
    render(args)
      .map_err(|e| {
        RpcError::new(
          INTERNAL_ERROR,
          truncate_detail(format!("prompt failed: {e}")),
        )
      })
      .map(move |messages| {
        let mut obj = Map::new();
        obj.insert("resultType".into(), json!("complete"));
        if let Some(d) = description {
          obj.insert("description".into(), Value::String(d));
        }
        obj.insert(
          "messages".into(),
          Value::Array(messages.iter().map(PromptMessage::to_json).collect()),
        );
        Value::Object(obj)
      })
  }
}

fn done(result: Result<Value, RpcError>) -> Effect<Value, RpcError> {
  match result {
    Ok(v) => succeed(v),
    Err(e) => fail(e),
  }
}

/// Checks the per-request `_meta` the pinned revision requires: the protocol version (the
/// pinned one, else [`UNSUPPORTED_PROTOCOL_VERSION`]) and the client capabilities.
pub(crate) fn check_meta(params: &Value) -> Result<(), RpcError> {
  let meta = params.get("_meta");
  let mut missing = Vec::new();
  match meta.and_then(|m| m.get(META_PROTOCOL_VERSION)) {
    Some(Value::String(v)) if v == PROTOCOL_VERSION => {}
    Some(Value::String(v)) => return Err(unsupported_version(v)),
    _ => missing.push(META_PROTOCOL_VERSION),
  }
  if !meta
    .and_then(|m| m.get(META_CLIENT_CAPABILITIES))
    .is_some_and(Value::is_object)
  {
    missing.push(META_CLIENT_CAPABILITIES);
  }
  if missing.is_empty() {
    Ok(())
  } else {
    Err(RpcError::invalid_params(format!(
      "request _meta is missing or has malformed required fields: {}",
      missing.join(", ")
    )))
  }
}

/// [`UNSUPPORTED_PROTOCOL_VERSION`] for `requested`, listing the pinned revision.
pub(crate) fn unsupported_version(requested: &str) -> RpcError {
  RpcError::new(UNSUPPORTED_PROTOCOL_VERSION, "Unsupported protocol version").with_data(json!({
    "requested": truncate_detail(requested.to_owned()),
    "supported": [PROTOCOL_VERSION],
  }))
}

/// One request running as a fiber: resolves to its response (`None` when it was interrupted,
/// which a cancelled request is); interrupts the fiber when dropped early.
pub(crate) struct RunningRequest {
  id: RequestId,
  fiber: Arc<FiberContext>,
  join: Option<JoinFuture<Value, RpcError>>,
}

impl RunningRequest {
  /// The request's fiber (to interrupt it from a `notifications/cancelled`).
  pub(crate) fn fiber(&self) -> Arc<FiberContext> {
    Arc::clone(&self.fiber)
  }
}

impl Future for RunningRequest {
  type Output = Option<Message>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Message>> {
    let Some(join) = self.join.as_mut() else {
      return Poll::Ready(None);
    };
    let exit = std::task::ready!(Pin::new(join).poll(cx));
    self.join = None;
    Poll::Ready(exit_message(self.id.clone(), exit))
  }
}

impl Drop for RunningRequest {
  fn drop(&mut self) {
    if self.join.is_some() {
      self.fiber.request_interrupt();
    }
  }
}

/// The response for a request fiber's exit: its result, its typed error, `None` when it was
/// interrupted, and [`RpcError::internal`] (no detail) for a defect.
pub(crate) fn exit_message(id: RequestId, exit: Exit<Value, RpcError>) -> Option<Message> {
  match exit {
    Exit::Success(v) => Some(Message::response(id, v)),
    Exit::Failure(cause) => match first_failure(cause) {
      Ok(e) => Some(Message::error(Some(id), e)),
      Err(rest) if rest.contains(&|c| matches!(c, Cause::Die(_))) => {
        Some(Message::error(Some(id), RpcError::internal()))
      }
      Err(_) => None,
    },
  }
}

fn first_failure<E>(cause: Cause<E>) -> Result<E, Cause<E>> {
  match cause {
    Cause::Fail(e) => Ok(e),
    Cause::Both(l, r) | Cause::Then(l, r) => match first_failure(*l) {
      Ok(e) => Ok(e),
      Err(l) => first_failure(*r).map_err(|r| Cause::Both(Box::new(l), Box::new(r))),
    },
    other => Err(other),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{FiberId, Schema, die};

  #[derive(Clone, Debug, Schema)]
  struct Empty {}

  #[derive(Clone, Debug, Schema)]
  struct Out {
    n: i64,
  }

  fn meta() -> Value {
    json!({ META_PROTOCOL_VERSION: PROTOCOL_VERSION, META_CLIENT_CAPABILITIES: {} })
  }

  fn req(method: &str, mut params: Value) -> Request {
    params["_meta"] = meta();
    Request {
      id: RequestId::Number(1),
      method: method.into(),
      params: Some(params),
    }
  }

  fn run(server: &McpServer, method: &str, params: Value) -> Result<Value, RpcError> {
    server
      .answer(req(method, params), Arc::from("p"), None)
      .block_on()
      .into_result()
      .map_err(|c| c.fail_option().expect("typed failure"))
  }

  fn server() -> McpServer {
    let tools = ToolSet::new()
      .with(crate::tools::Tool::new(
        "one",
        "1",
        |_: Empty| -> Effect<Out, String> { succeed(Out { n: 1 }) },
      ))
      .unwrap()
      .with(
        crate::tools::Tool::new("wipe", "w", |_: Empty| -> Effect<Out, String> {
          succeed(Out { n: 0 })
        })
        .writes(),
      )
      .unwrap()
      .with(crate::tools::Tool::new(
        "boom",
        "b",
        |_: Empty| -> Effect<Out, String> { fail("exploded".to_owned()) },
      ))
      .unwrap();
    McpServer::new("s", "1")
      .with_tools(tools)
      .with_limits(McpLimits::default().with_page_size(1))
  }

  #[test]
  fn meta_is_required_and_the_version_pinned() {
    let s = server();
    let no_meta = Request {
      id: RequestId::Number(1),
      method: "tools/list".into(),
      params: None,
    };
    let e = s
      .answer(no_meta, Arc::from("p"), None)
      .block_on()
      .into_result()
      .unwrap_err()
      .fail_option()
      .unwrap();
    assert_eq!(e.code, super::super::message::INVALID_PARAMS);
    assert!(e.message.contains(META_PROTOCOL_VERSION));
    assert!(e.message.contains(META_CLIENT_CAPABILITIES));
    let old =
      json!({ "_meta": { META_PROTOCOL_VERSION: "2025-06-18", META_CLIENT_CAPABILITIES: {} } });
    let e = check_meta(&old).unwrap_err();
    assert_eq!(e.code, UNSUPPORTED_PROTOCOL_VERSION);
    assert_eq!(e.data.unwrap()["supported"], json!([PROTOCOL_VERSION]));
    assert!(check_meta(&json!({ "_meta": meta() })).is_ok());
    let e = run(&s, "ping", json!({})).unwrap_err();
    assert_eq!(e.code, super::super::message::METHOD_NOT_FOUND);
  }

  #[test]
  fn lists_page_through_principal_bound_cursors() {
    let s = server();
    let first = run(&s, "tools/list", json!({})).unwrap();
    assert_eq!(first["tools"].as_array().unwrap().len(), 1);
    let cursor = first["nextCursor"].as_str().unwrap().to_owned();
    let second = run(&s, "tools/list", json!({ "cursor": cursor })).unwrap();
    assert!(second.get("nextCursor").is_none());
    let names: Vec<_> = [&first, &second]
      .iter()
      .map(|p| p["tools"][0]["name"].as_str().unwrap().to_owned())
      .collect();
    // The write tool is not listed without the opt-out.
    assert_eq!(names, ["boom", "one"]);
    // Another principal, another list, or a non-string cursor is refused.
    let other = s
      .answer(
        req("tools/list", json!({ "cursor": cursor })),
        Arc::from("q"),
        None,
      )
      .block_on()
      .into_result();
    assert!(other.is_err());
    assert!(run(&s, "prompts/list", json!({ "cursor": cursor })).is_err());
    assert!(run(&s, "tools/list", json!({ "cursor": 1 })).is_err());
    let empty = run(&s, "resources/templates/list", json!({})).unwrap();
    assert_eq!(empty["resourceTemplates"], json!([]));
    let full = McpServer::new("s", "1")
      .with_tools(s.tools().clone())
      .with_limits(
        McpLimits::default()
          .with_page_size(1)
          .with_max_handles(1, 1),
      );
    full.handles.mint(&Arc::from("z"), json!(1)).unwrap();
    assert_eq!(
      run(&full, "tools/list", json!({})).unwrap_err().code,
      SERVER_BUSY
    );
  }

  #[test]
  fn tool_calls_map_refusals_and_failures() {
    let s = server();
    let ok = run(&s, "tools/call", json!({ "name": "one" })).unwrap();
    assert_eq!(ok["structuredContent"], json!({ "n": 1 }));
    assert_eq!(ok["isError"], json!(false));
    let failed = run(&s, "tools/call", json!({ "name": "boom", "arguments": {} })).unwrap();
    assert_eq!(failed["isError"], json!(true));
    let denied = run(&s, "tools/call", json!({ "name": "wipe" })).unwrap();
    assert!(
      denied["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("not allowed")
    );
    assert!(run(&s, "tools/call", json!({ "name": "nope" })).is_err());
    assert!(run(&s, "tools/call", json!({})).is_err());
    assert!(run(&s, "tools/call", json!({ "name": "one", "arguments": [1] })).is_err());
    let writer = server().dangerous_mcp_write_tools();
    assert!(writer.writes_allowed());
    let wiped = run(&writer, "tools/call", json!({ "name": "wipe" })).unwrap();
    assert_eq!(wiped["structuredContent"], json!({ "n": 0 }));
    let small = server().with_limits(McpLimits::default().with_max_tool_output_bytes(2));
    let big = run(&small, "tools/call", json!({ "name": "one" })).unwrap();
    assert_eq!(big["isError"], json!(true));
  }

  #[test]
  fn discover_describes_what_is_registered() {
    let bare = McpServer::new("bare", "0").with_instructions("use it");
    let d = run(&bare, "server/discover", json!({})).unwrap();
    assert_eq!(d["capabilities"], json!({}));
    assert_eq!(d["instructions"], json!("use it"));
    assert_eq!(d["_meta"][SERVER_INFO_META_KEY]["name"], json!("bare"));
    let s = server()
      .with_info(Implementation::new("renamed", "2"))
      .with_resource(Resource::new("a://1", "one"), || {
        succeed::<_, String, _>(ResourceBody::Text("x".into()))
      })
      .with_prompt(Prompt::new("p"), |_| succeed::<_, String, _>(vec![]));
    let d = run(&s, "server/discover", json!({})).unwrap();
    assert_eq!(
      d["capabilities"],
      json!({ "tools": {}, "resources": {}, "prompts": {} })
    );
    assert_eq!(s.info().name, "renamed");
    assert_eq!(s.limits().page_size, 1);
    let dbg = format!("{s:?}");
    assert!(dbg.contains("a://1") && dbg.contains("renamed"));
  }

  #[test]
  fn limits_never_reach_zero() {
    let l = McpLimits::default()
      .with_max_message_bytes(0)
      .with_max_in_flight(0)
      .with_max_tool_output_bytes(0)
      .with_page_size(0)
      .with_max_handles(0, 0)
      .with_handle_ttl(Duration::ZERO);
    assert_eq!(
      (
        l.max_message_bytes,
        l.max_in_flight,
        l.max_tool_output_bytes,
        l.page_size,
        l.max_handles_per_principal,
        l.max_handles,
        l.handle_ttl
      ),
      (1, 1, 1, 1, 1, 1, Duration::from_secs(1))
    );
  }

  #[test]
  fn exits_map_to_responses() {
    let id = RequestId::Number(9);
    assert!(matches!(
      exit_message(id.clone(), Exit::Success(json!(1))),
      Some(Message::Response(_))
    ));
    let both = Cause::Both(
      Box::new(Cause::Interrupt(FiberId::fresh())),
      Box::new(Cause::Fail(RpcError::internal())),
    );
    assert!(matches!(
      exit_message(id.clone(), Exit::Failure(both)),
      Some(Message::Error(_))
    ));
    let interrupted = Cause::Then(
      Box::new(Cause::Interrupt(FiberId::fresh())),
      Box::new(Cause::Interrupt(FiberId::fresh())),
    );
    assert_eq!(exit_message(id.clone(), Exit::Failure(interrupted)), None);
    let died = die::<Value, RpcError, id_effect::HNil>("boom").block_on();
    let Some(Message::Error(e)) = exit_message(id, died) else {
      panic!("a defect answers an internal error");
    };
    assert_eq!(e.error, RpcError::internal());
  }
}
