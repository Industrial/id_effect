//! [`McpClient`]: calls an MCP server's tools, resources and prompts over stdio (any byte
//! stream pair) or Streamable HTTP (through `id_effect_platform`'s client, so the SSRF policy,
//! timeouts and body caps apply: ADR 0013 R6).
//!
//! Every request carries the pinned revision, the client's identity and its (empty)
//! capabilities in `_meta`; the client offers no sampling, elicitation or roots. Interrupting a
//! call cancels it: over stdio the client sends `notifications/cancelled`, over HTTP it closes
//! the response stream.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{Effect, fail, from_async, succeed};
use id_effect_platform::http::{
  HttpBody, HttpClient, HttpClientConfig, HttpRequest, ReqwestHttpClient,
};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::error::McpError;
use super::headers::{HEADER_PROTOCOL_VERSION, request_headers};
use super::message::{Message, RequestId, RpcError, decode_message_with_limit};
use super::stdio::{Line, read_line, write_lines};
use super::types::{
  Implementation, McpTool, Progress, Prompt, PromptReply, Resource, ResourceContents,
  ServerDescription, ToolResult,
};
use super::{
  DEFAULT_MAX_MESSAGE_BYTES, META_CLIENT_CAPABILITIES, META_CLIENT_INFO, META_PROTOCOL_VERSION,
  METHOD_NOT_FOUND, NOTIFICATION_CANCELLED, NOTIFICATION_PROGRESS, PROTOCOL_VERSION,
};
use crate::stream::SseParser;

/// Most pages a `list_*` call follows before it gives up (a server cannot loop a client).
pub const MAX_LIST_PAGES: usize = 64;

/// Default cap on one streamed (SSE) HTTP response: 16 MiB.
pub const DEFAULT_MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

/// Receives the progress notifications of one call.
pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

struct Pending {
  reply: oneshot::Sender<Result<Value, McpError>>,
  progress: Option<ProgressFn>,
}

#[derive(Default)]
struct PendingState {
  calls: HashMap<RequestId, Pending>,
  closed: bool,
}

type PendingMap = Arc<Mutex<PendingState>>;

fn lock(map: &PendingMap) -> std::sync::MutexGuard<'_, PendingState> {
  map
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone)]
struct IoConn {
  tx: mpsc::Sender<Option<String>>,
  pending: PendingMap,
}

#[derive(Clone)]
struct HttpConn {
  url: String,
  http: Arc<dyn HttpClient>,
  timeout: Option<Duration>,
  max_stream_bytes: usize,
  headers: Vec<(String, String)>,
}

/// Headers the protocol owns: a caller cannot set or override them (B23).
fn reserved_header(name: &str) -> bool {
  matches!(
    name,
    "content-type" | "content-length" | "accept" | "host" | "transfer-encoding" | "connection"
  ) || name.starts_with("mcp-")
}

/// Checks a caller's header: a token name the protocol does not own, a value of visible ASCII,
/// spaces and tabs.
fn check_header(name: &str, value: &str) -> Result<String, McpError> {
  let lower = name.to_ascii_lowercase();
  let token = !lower.is_empty()
    && lower
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
  if !token {
    return Err(McpError::InvalidHeader("invalid header name".into()));
  }
  if reserved_header(&lower) {
    return Err(McpError::InvalidHeader(format!(
      "`{lower}` is set by the protocol"
    )));
  }
  if !value
    .bytes()
    .all(|b| b == b'\t' || (0x20..0x7f).contains(&b))
  {
    return Err(McpError::InvalidHeader(format!(
      "invalid value for `{lower}`"
    )));
  }
  Ok(lower)
}

#[derive(Clone)]
enum Conn {
  Io(IoConn),
  Http(HttpConn),
}

/// An MCP client (see the [module docs](self)). Clones share the connection.
///
/// ```no_run
/// use id_effect_ai::mcp::McpClient;
/// use id_effect_platform::http::HttpClientConfig;
///
/// // A remote server: the default SSRF policy applies (public addresses only).
/// let client = McpClient::http("https://tools.example.com/mcp", &HttpClientConfig::default());
/// let tools = client.list_tools().block_on().into_result();
/// # let _ = tools;
/// ```
#[derive(Clone)]
pub struct McpClient {
  conn: Arc<Conn>,
  info: Implementation,
  next_id: Arc<AtomicI64>,
  max_message_bytes: usize,
  schemas: Arc<Mutex<HashMap<String, Value>>>,
}

impl fmt::Debug for McpClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let transport = match &*self.conn {
      Conn::Io(_) => "stdio".to_owned(),
      Conn::Http(h) => h.url.clone(),
    };
    f.debug_struct("McpClient")
      .field("transport", &transport)
      .field("info", &self.info)
      .finish_non_exhaustive()
  }
}

impl McpClient {
  fn with_conn(conn: Conn) -> Self {
    Self {
      conn: Arc::new(conn),
      info: Implementation::new("id_effect_ai", env!("CARGO_PKG_VERSION")),
      next_id: Arc::new(AtomicI64::new(1)),
      max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
      schemas: Arc::default(),
    }
  }

  /// A client of the Streamable HTTP endpoint at `url`, through a platform client built from
  /// `config` (its SSRF policy decides which addresses may be reached).
  pub fn http(url: impl Into<String>, config: &HttpClientConfig) -> Self {
    Self::http_with_client(url, Arc::new(ReqwestHttpClient::from_config(config)))
  }

  /// A client of the Streamable HTTP endpoint at `url` through `http`.
  pub fn http_with_client(url: impl Into<String>, http: Arc<dyn HttpClient>) -> Self {
    Self::with_conn(Conn::Http(HttpConn {
      url: url.into(),
      http,
      timeout: None,
      max_stream_bytes: DEFAULT_MAX_STREAM_BYTES,
      headers: Vec::new(),
    }))
  }

  /// HTTP only: sends `name: value` with every request (an API key header, for instance).
  /// Refused: an invalid name or value, and the headers the protocol owns (`content-type`,
  /// `accept`, `mcp-*`, ...), so a caller can never steer routing (B23).
  pub fn with_header(mut self, name: &str, value: &str) -> Result<Self, McpError> {
    let name = check_header(name, value)?;
    if let Conn::Http(h) = Arc::make_mut(&mut self.conn) {
      h.headers.retain(|(k, _)| *k != name);
      h.headers.push((name, value.to_owned()));
    }
    Ok(self)
  }

  /// HTTP only: sends `token` as a bearer token in the `Authorization` header of every request.
  pub fn with_bearer_token(self, token: &str) -> Result<Self, McpError> {
    self.with_header("authorization", &format!("Bearer {token}"))
  }

  /// A client speaking newline-delimited JSON-RPC over `reader` and `writer` (a child
  /// process's stdout and stdin, a socket, an in-memory pipe). Spawns the reader and writer
  /// tasks on the running tokio runtime.
  pub fn over_io<R, W>(reader: R, writer: W) -> Effect<McpClient, McpError>
  where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
  {
    Effect::from_ctx(move |ctx| {
      let exec = Arc::clone(ctx.fiber().executor());
      async move {
        let (tx, rx) = mpsc::channel::<Option<String>>(256);
        let pending: PendingMap = Arc::default();
        exec.spawn(Box::pin(async move {
          let _ = write_lines(writer, rx).await;
        }));
        exec.spawn(Box::pin(read_loop(
          reader,
          DEFAULT_MAX_MESSAGE_BYTES,
          Arc::clone(&pending),
          // Weak: dropping every client closes the writer, so the server sees end of input.
          tx.downgrade(),
        )));
        Ok(Self::with_conn(Conn::Io(IoConn { tx, pending })))
      }
    })
  }

  /// Replaces the identity sent with every request.
  #[must_use]
  pub fn with_client_info(mut self, info: Implementation) -> Self {
    self.info = info;
    self
  }

  /// Caps one message sent or received (default 1 MiB).
  #[must_use]
  pub fn with_max_message_bytes(mut self, max: usize) -> Self {
    self.max_message_bytes = max.max(1);
    self
  }

  /// HTTP only: a timeout for each request (on top of the platform client's).
  #[must_use]
  pub fn with_timeout(mut self, timeout: Duration) -> Self {
    if let Conn::Http(h) = Arc::make_mut(&mut self.conn) {
      h.timeout = Some(timeout);
    }
    self
  }

  /// Sends request `method` with `params` (an object; `_meta` is added) and returns its
  /// result, for methods this client has no typed call for.
  pub fn request(&self, method: &str, params: Value) -> Effect<Value, McpError> {
    self.send(method, params, None)
  }

  /// `server/discover`: the server's revisions, capabilities and identity.
  pub fn discover(&self) -> Effect<ServerDescription, McpError> {
    self
      .request("server/discover", json!({}))
      .map(|v| ServerDescription::from_json(&v))
  }

  /// Every tool the server lists (following cursors, at most [`MAX_LIST_PAGES`] pages).
  pub fn list_tools(&self) -> Effect<Vec<McpTool>, McpError> {
    let schemas = Arc::clone(&self.schemas);
    self.list_all("tools/list", "tools").flat_map(move |items| {
      let tools: Option<Vec<McpTool>> = items.iter().map(McpTool::from_json).collect();
      match tools {
        Some(tools) => {
          let mut cache = schemas
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
          for t in &tools {
            cache.insert(t.name.clone(), t.input_schema.clone());
          }
          succeed(tools)
        }
        None => fail(McpError::Protocol("invalid tool in tools/list".into())),
      }
    })
  }

  /// Calls tool `name` with `arguments` (an object). A tool's own failure is a [`ToolResult`]
  /// with `is_error`, not an `Err`.
  pub fn call_tool(&self, name: &str, arguments: Value) -> Effect<ToolResult, McpError> {
    self.call(name, arguments, None)
  }

  /// [`call_tool`](Self::call_tool), receiving the call's progress notifications.
  pub fn call_tool_with_progress<F>(
    &self,
    name: &str,
    arguments: Value,
    on_progress: F,
  ) -> Effect<ToolResult, McpError>
  where
    F: Fn(Progress) + Send + Sync + 'static,
  {
    self.call(name, arguments, Some(Arc::new(on_progress)))
  }

  fn call(
    &self,
    name: &str,
    arguments: Value,
    progress: Option<ProgressFn>,
  ) -> Effect<ToolResult, McpError> {
    self
      .send(
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
        progress,
      )
      .map(|v| ToolResult::from_json(&v))
  }

  /// Every resource the server lists.
  pub fn list_resources(&self) -> Effect<Vec<Resource>, McpError> {
    self
      .list_all("resources/list", "resources")
      .flat_map(|items| collect(&items, Resource::from_json, "resource"))
  }

  /// Reads resource `uri`.
  pub fn read_resource(&self, uri: &str) -> Effect<Vec<ResourceContents>, McpError> {
    self
      .request("resources/read", json!({ "uri": uri }))
      .flat_map(|v| {
        let items = v
          .get("contents")
          .and_then(Value::as_array)
          .cloned()
          .unwrap_or_default();
        collect(&items, ResourceContents::from_json, "resource contents")
      })
  }

  /// Every prompt the server lists.
  pub fn list_prompts(&self) -> Effect<Vec<Prompt>, McpError> {
    self
      .list_all("prompts/list", "prompts")
      .flat_map(|items| collect(&items, Prompt::from_json, "prompt"))
  }

  /// Renders prompt `name` with `arguments`.
  pub fn get_prompt(
    &self,
    name: &str,
    arguments: BTreeMap<String, String>,
  ) -> Effect<PromptReply, McpError> {
    self
      .request(
        "prompts/get",
        json!({ "name": name, "arguments": arguments }),
      )
      .map(|v| PromptReply::from_json(&v))
  }

  fn list_all(&self, method: &'static str, key: &'static str) -> Effect<Vec<Value>, McpError> {
    self.list_from(method, key, None, Vec::new(), 0)
  }

  fn list_from(
    &self,
    method: &'static str,
    key: &'static str,
    cursor: Option<String>,
    mut acc: Vec<Value>,
    pages: usize,
  ) -> Effect<Vec<Value>, McpError> {
    let params = cursor.map_or_else(|| json!({}), |c| json!({ "cursor": c }));
    let this = self.clone();
    self.request(method, params).flat_map(move |page| {
      acc.extend(
        page
          .get(key)
          .and_then(Value::as_array)
          .cloned()
          .unwrap_or_default(),
      );
      match page.get("nextCursor").and_then(Value::as_str) {
        None => succeed(acc),
        Some(_) if pages + 1 >= MAX_LIST_PAGES => fail(McpError::Protocol(format!(
          "{method} returned more than {MAX_LIST_PAGES} pages"
        ))),
        Some(next) => this.list_from(method, key, Some(next.to_owned()), acc, pages + 1),
      }
    })
  }

  fn send(
    &self,
    method: &str,
    params: Value,
    progress: Option<ProgressFn>,
  ) -> Effect<Value, McpError> {
    let n = self.next_id.fetch_add(1, Ordering::Relaxed);
    let id = RequestId::Number(n);
    let mut params = match params {
      Value::Object(obj) => obj,
      _ => Map::new(),
    };
    let mut meta = Map::new();
    meta.insert(META_PROTOCOL_VERSION.into(), json!(PROTOCOL_VERSION));
    meta.insert(META_CLIENT_INFO.into(), self.info.to_json());
    meta.insert(META_CLIENT_CAPABILITIES.into(), json!({}));
    if progress.is_some() {
      meta.insert("progressToken".into(), json!(n));
    }
    params.insert("_meta".into(), Value::Object(meta));
    let msg = Message::request(id.clone(), method, Some(Value::Object(params)));
    let text = msg.to_text();
    if text.len() > self.max_message_bytes {
      return fail(McpError::TooLarge {
        max: self.max_message_bytes,
      });
    }
    match &*self.conn {
      Conn::Io(_) => {
        let conn = Arc::clone(&self.conn);
        from_async(move || async move {
          let Conn::Io(io) = &*conn else {
            unreachable!("an io connection stays an io connection")
          };
          io_call(io, id, text, progress).await
        })
      }
      Conn::Http(http) => {
        let schema = match &msg {
          Message::Request(r) if r.method == "tools/call" => r
            .params
            .as_ref()
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .and_then(|name| {
              self
                .schemas
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(name)
                .cloned()
            }),
          _ => None,
        };
        let mut req = HttpRequest::post(http.url.clone(), text.into_bytes());
        req.headers = vec![
          ("content-type".into(), "application/json".into()),
          (
            "accept".into(),
            "application/json, text/event-stream".into(),
          ),
          (HEADER_PROTOCOL_VERSION.into(), PROTOCOL_VERSION.into()),
        ];
        req.headers.extend(request_headers(&msg, schema.as_ref()));
        req.headers.extend(http.headers.iter().cloned());
        req.timeout = http.timeout;
        req.max_body_bytes = Some(http.max_stream_bytes);
        let max = self.max_message_bytes;
        http
          .http
          .execute_stream(req)
          .map_err(McpError::from)
          .flat_map(move |resp| {
            let content_type = resp
              .headers
              .iter()
              .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
              .map(|(_, v)| v.to_ascii_lowercase())
              .unwrap_or_default();
            let call = HttpCall { id, progress, max };
            if content_type.starts_with("text/event-stream") && (200..300).contains(&resp.status) {
              read_sse(resp.body, SseParser::new(), call)
            } else {
              let status = resp.status;
              resp
                .body
                .collect()
                .map_err(McpError::from)
                .flat_map(move |body| match call.json_answer(status, &body) {
                  Ok(v) => succeed(v),
                  Err(e) => fail(e),
                })
            }
          })
      }
    }
  }
}

fn collect<T>(
  items: &[Value],
  parse: fn(&Value) -> Option<T>,
  what: &str,
) -> Effect<Vec<T>, McpError>
where
  T: Send + 'static,
{
  match items.iter().map(parse).collect::<Option<Vec<T>>>() {
    Some(v) => succeed(v),
    None => fail(McpError::Protocol(format!("invalid {what} in the answer"))),
  }
}

/// One HTTP call's id, progress callback and message cap.
struct HttpCall {
  id: RequestId,
  progress: Option<ProgressFn>,
  max: usize,
}

impl HttpCall {
  /// What one message from the server means for this call: `Some` ends it.
  fn on_message(&self, msg: Message) -> Option<Result<Value, McpError>> {
    match msg {
      Message::Response(r) if r.id == self.id => Some(Ok(r.result)),
      Message::Error(e) if e.id.is_none() || e.id.as_ref() == Some(&self.id) => {
        Some(Err(McpError::Rpc(e.error)))
      }
      Message::Notification(n) if n.method == NOTIFICATION_PROGRESS => {
        if let (Some(cb), Some(p)) = (
          &self.progress,
          n.params.as_ref().and_then(Progress::from_json),
        ) {
          cb(p);
        }
        None
      }
      _ => None,
    }
  }

  /// The answer of a buffered (non-SSE) response.
  fn json_answer(&self, status: u16, body: &[u8]) -> Result<Value, McpError> {
    let parsed = decode_message_with_limit(body, self.max);
    match parsed {
      Ok(msg) => match self.on_message(msg) {
        Some(answer) => answer,
        None => Err(McpError::Protocol(
          "the response does not answer the request".into(),
        )),
      },
      Err(_) if !(200..300).contains(&status) => Err(McpError::Http {
        status,
        detail: crate::error::truncate_detail(String::from_utf8_lossy(body).into_owned()),
      }),
      Err(e) => Err(McpError::Protocol(e.to_string())),
    }
  }
}

/// Reads server-sent events until the call's response arrives.
fn read_sse(body: HttpBody, mut parser: SseParser, call: HttpCall) -> Effect<Value, McpError> {
  let next = body.next_chunk().map_err(McpError::from);
  next.flat_map(move |chunk| {
    let Some(bytes) = chunk else {
      return fail(McpError::Protocol(
        "the event stream ended before the response".into(),
      ));
    };
    for event in parser.feed_bytes(&bytes) {
      let data = event.data();
      if data.is_empty() {
        continue;
      }
      match decode_message_with_limit(data.as_bytes(), call.max) {
        Ok(msg) => match call.on_message(msg) {
          Some(Ok(v)) => return succeed(v),
          Some(Err(e)) => return fail(e),
          None => {}
        },
        Err(e) => return fail(McpError::Protocol(e.to_string())),
      }
    }
    if parser.pending_bytes() > call.max {
      return fail(McpError::TooLarge { max: call.max });
    }
    read_sse(body, parser, call)
  })
}

/// Sends `text` (request `id`) and waits for its answer; dropped early, it cancels the request.
async fn io_call(
  io: &IoConn,
  id: RequestId,
  text: String,
  progress: Option<ProgressFn>,
) -> Result<Value, McpError> {
  let (reply, answer) = oneshot::channel();
  {
    let mut state = lock(&io.pending);
    if state.closed {
      return Err(McpError::transport("connection closed"));
    }
    state.calls.insert(id.clone(), Pending { reply, progress });
  }
  let mut guard = CancelOnDrop {
    tx: io.tx.clone(),
    pending: Arc::clone(&io.pending),
    id: Some(id),
  };
  if io.tx.send(Some(text)).await.is_err() {
    guard.disarm();
    return Err(McpError::transport("connection closed"));
  }
  let out = answer
    .await
    .unwrap_or_else(|_| Err(McpError::transport("connection closed")));
  guard.id = None;
  out
}

/// Cancels a request whose call was dropped before its answer: forgets it and sends
/// `notifications/cancelled`.
struct CancelOnDrop {
  tx: mpsc::Sender<Option<String>>,
  pending: PendingMap,
  id: Option<RequestId>,
}

impl CancelOnDrop {
  fn disarm(&mut self) {
    if let Some(id) = self.id.take() {
      lock(&self.pending).calls.remove(&id);
    }
  }
}

impl Drop for CancelOnDrop {
  fn drop(&mut self) {
    if let Some(id) = self.id.take() {
      lock(&self.pending).calls.remove(&id);
      let note = Message::notification(
        NOTIFICATION_CANCELLED,
        Some(json!({ "requestId": id.to_json(), "reason": "cancelled by the client" })),
      );
      let _ = self.tx.try_send(Some(note.to_text()));
    }
  }
}

/// Routes the server's messages to the waiting calls until the connection ends, then fails
/// every call still waiting.
async fn read_loop<R: AsyncRead + Unpin>(
  reader: R,
  max: usize,
  pending: PendingMap,
  tx: mpsc::WeakSender<Option<String>>,
) {
  let mut reader = BufReader::new(reader);
  loop {
    let bytes = match read_line(&mut reader, max).await {
      Ok(Line::Text(bytes)) => bytes,
      // A line over the cap cannot be matched to a call; it is dropped.
      Ok(Line::TooLong) => continue,
      Ok(Line::Eof) | Err(_) => break,
    };
    let Ok(msg) = decode_message_with_limit(&bytes, max) else {
      continue;
    };
    match msg {
      Message::Response(r) => answer(&pending, &r.id, Ok(r.result)),
      Message::Error(e) => {
        if let Some(id) = &e.id {
          answer(&pending, id, Err(McpError::Rpc(e.error)));
        }
      }
      Message::Notification(n) if n.method == NOTIFICATION_PROGRESS => {
        let params = n.params.unwrap_or_default();
        let id = params.get("progressToken").and_then(RequestId::from_json);
        let cb = id.and_then(|id| {
          lock(&pending)
            .calls
            .get(&id)
            .and_then(|p| p.progress.clone())
        });
        if let (Some(cb), Some(p)) = (cb, Progress::from_json(&params)) {
          cb(p);
        }
      }
      Message::Notification(_) => {}
      // The client offers no capabilities: every server request is refused.
      Message::Request(r) => {
        let refusal = Message::error(
          Some(r.id),
          RpcError::new(METHOD_NOT_FOUND, "Method not found"),
        );
        if let Some(tx) = tx.upgrade() {
          let _ = tx.try_send(Some(refusal.to_text()));
        }
      }
    }
  }
  let calls = {
    let mut state = lock(&pending);
    state.closed = true;
    std::mem::take(&mut state.calls)
  };
  for (_, call) in calls {
    let _ = call
      .reply
      .send(Err(McpError::transport("connection closed")));
  }
}

fn answer(pending: &PendingMap, id: &RequestId, result: Result<Value, McpError>) {
  if let Some(call) = lock(pending).calls.remove(id) {
    let _ = call.reply.send(result);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_platform::error::HttpError;
  use id_effect_platform::http::{HttpResponse, StreamingHttpResponse};

  /// Answers every request with one canned response.
  struct Canned {
    status: u16,
    content_type: &'static str,
    chunks: Vec<Vec<u8>>,
  }

  impl HttpClient for Canned {
    fn execute(&self, _req: HttpRequest) -> Effect<HttpResponse, HttpError> {
      fail(HttpError::InvalidRequest("unused".into()))
    }

    fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
      succeed(StreamingHttpResponse {
        status: self.status,
        headers: vec![("Content-Type".into(), self.content_type.into())],
        body: HttpBody::from_chunks(self.chunks.clone()),
      })
    }
  }

  fn client(status: u16, content_type: &'static str, chunks: &[&str]) -> McpClient {
    McpClient::http_with_client(
      "http://unused",
      Arc::new(Canned {
        status,
        content_type,
        chunks: chunks.iter().map(|c| c.as_bytes().to_vec()).collect(),
      }),
    )
    .with_timeout(Duration::from_secs(5))
  }

  fn err_of(eff: Effect<Value, McpError>) -> McpError {
    eff
      .block_on()
      .into_result()
      .unwrap_err()
      .fail_option()
      .unwrap()
  }

  #[test]
  fn sse_answers_progress_and_errors() {
    let seen: Arc<Mutex<Vec<f64>>> = Arc::default();
    let s = Arc::clone(&seen);
    let c = client(
      200,
      "text/event-stream",
      &[
        ": comment\n\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",",
        "\"params\":{\"progressToken\":1,\"progress\":0.5}}\n\n",
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"other\"}\n\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[]}}\n\n",
      ],
    );
    let out = c
      .call_tool_with_progress("t", json!({}), move |p| s.lock().unwrap().push(p.progress))
      .block_on()
      .into_result()
      .unwrap();
    assert!(!out.is_error);
    assert_eq!(*seen.lock().unwrap(), vec![0.5]);

    let e = client(
      200,
      "text/event-stream",
      &["data: {\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32602,\"message\":\"x\"}}\n\n"],
    );
    assert!(matches!(
      err_of(e.request("m", json!({}))),
      McpError::Rpc(_)
    ));
    let ended = client(200, "text/event-stream", &["data: \n\n"]);
    assert!(matches!(
      err_of(ended.request("m", json!({}))),
      McpError::Protocol(_)
    ));
    let garbage = client(200, "text/event-stream", &["data: nope\n\n"]);
    assert!(matches!(
      err_of(garbage.request("m", json!({}))),
      McpError::Protocol(_)
    ));
    let huge = client(200, "text/event-stream", &["data: 0123456789"]).with_max_message_bytes(4);
    assert!(matches!(
      err_of(huge.request("m", json!({}))),
      McpError::TooLarge { .. }
    ));
  }

  #[test]
  fn json_answers_and_http_failures() {
    let ok = client(
      200,
      "application/json",
      &[r#"{"jsonrpc":"2.0","id":1,"result":{"x":1}}"#],
    );
    assert_eq!(
      ok.request("m", json!([])).block_on().into_result().unwrap(),
      json!({ "x": 1 })
    );
    let other = client(
      200,
      "application/json",
      &[r#"{"jsonrpc":"2.0","id":9,"result":{}}"#],
    );
    assert!(matches!(
      err_of(other.request("m", json!({}))),
      McpError::Protocol(_)
    ));
    let rpc = client(
      400,
      "application/json",
      &[r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32020,"message":"mismatch"}}"#],
    );
    assert_eq!(
      err_of(rpc.request("m", json!({}))).rpc().unwrap().code,
      -32020
    );
    let status = client(403, "text/plain", &["forbidden"]);
    assert_eq!(
      err_of(status.request("m", json!({}))),
      McpError::Http {
        status: 403,
        detail: "forbidden".into()
      }
    );
    let bad = client(200, "application/json", &["{"]);
    assert!(matches!(
      err_of(bad.request("m", json!({}))),
      McpError::Protocol(_)
    ));
    let small = ok.clone().with_max_message_bytes(8);
    assert_eq!(
      err_of(small.request("m", json!({}))),
      McpError::TooLarge { max: 8 }
    );
    assert!(format!("{ok:?}").contains("http://unused"));
  }

  #[test]
  fn typed_calls_refuse_malformed_answers() {
    let bad_tools = client(
      200,
      "application/json",
      &[r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":1}]}}"#],
    );
    let e = bad_tools.list_tools().block_on().into_result().unwrap_err();
    assert!(matches!(e.fail_option(), Some(McpError::Protocol(_))));
    let bad_resources = client(
      200,
      "application/json",
      &[r#"{"jsonrpc":"2.0","id":1,"result":{"resources":[{}]}}"#],
    );
    assert!(
      bad_resources
        .list_resources()
        .block_on()
        .into_result()
        .is_err()
    );
    let looping = client(
      200,
      "application/json",
      &[r#"{"jsonrpc":"2.0","id":1,"result":{"prompts":[],"nextCursor":"c"}}"#],
    );
    // Every page answers id 1, so the second page is refused as not answering its request.
    assert!(looping.list_prompts().block_on().into_result().is_err());
  }
}
