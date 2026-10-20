//! AsyncAPI 3.0 from the service contract (web.rpc #7): the stream kinds over WebSocket frames
//! and server-sent events, and the channels' topics.
//!
//! [`document`] describes the **server application** (an operation's `action` is from the
//! server's side: it `receive`s what clients send and `send`s what clients receive):
//!
//! | Kind | WebSocket channel `<Service>-v<N>-<proc>.ws` | SSE channel `<Service>-v<N>-<proc>.sse` |
//! |---|---|---|
//! | server stream | receives `call` (the input), `credit`, `cancel`; sends `item`…, `exit` | `GET {path}/{tag}?input=…`, events `item`…, `exit` |
//! | client stream | receives `call`, `item`…, `end`, `cancel`; sends `credit`, `exit` (the output) | — |
//! | two-way stream | receives `call`, `item`, `end`, `credit`, `cancel`; sends `item`, `credit`, `exit` | — |
//! | subscription | receives `call` (the input, optional `cursor`), `credit`, `cancel`; sends `item` (with `cursor`), `exit` | events with `id:` = cursor, resumed by `Last-Event-ID` |
//!
//! Every WebSocket channel rides one connection (the `ws` server, subprotocol
//! `idrpc.json.v1` or `idrpc.postcard.v1`), multiplexed by call id; the channel's address is the
//! procedure tag the `call` frame names. The `connection` channel holds the frames of call id 0:
//! `hello`, `resume`, `ping` and `pong`. Queries and mutations can be called on the same
//! connection (`call` → `exit`); [`crate::openapi`] describes them.
//!
//! **Cursors.** A cursor is the string `c1.` + 12 base64url characters, opaque to clients. On
//! WebSocket it is the `cursor` member of server `item` frames (an event-log position for
//! subscriptions, a per-session sequence for resumable streams) and of a subscription's `call`
//! frame (start after it); on SSE it is the event `id:`, sent back as `Last-Event-ID`. It is not
//! part of the procedure's output schema. A cursor is not a capability: resuming a session needs
//! its `SessionId`, bound to the authenticated principal (C8).
//!
//! **Topics.** [`ChannelTopic`]s add a channel per topic (`topic.<name>.ws`, `topic.<name>.sse`).
//! A topic is joined by calling its join subscription procedure (the address: the procedure tag on
//! WebSocket, `GET {path}/{tag}?input={"topic":…}` on SSE) and delivers the `channels`
//! `ChannelEvent`s: `joined`, `message` (the body is the topic's payload) and `presence`
//! (`joins` / `leaves` lists of members); every join goes through the topic's `JoinPolicy`
//! (deny by default).
//!
//! Payload schemas are the contract's JSON Schema 2020-12 schemas (`$schema` dropped, `$defs`
//! hoisted into `components/schemas` under the names [`crate::openapi`] documents), written as
//! AsyncAPI Schema Objects; error unions are `oneOf` with `discriminator: "_tag"`. Output is
//! deterministic: sorted keys, procedures in tag order, versions ascending.
//!
//! ```rust
//! use id_effect_rpc::asyncapi::{AsyncApiOptions, document};
//! use id_effect_rpc::contract::Contract;
//!
//! let contract = Contract::from_json(include_str!("../rpc.contract.json")).unwrap();
//! let doc = document(&contract, &AsyncApiOptions::new("Todos events", "1.0.0"));
//! assert_eq!(doc["asyncapi"], "3.0.0");
//! assert_eq!(doc["channels"]["Todos-v1-ticks.ws"]["address"], "Todos.ticks");
//! assert!(doc["channels"]["Todos-v1-ticks.sse"].is_object());
//! assert!(doc["channels"]["Todos-v1-shout.ws"]["messages"]["clientItem"].is_object());
//! ```

use serde_json::{Map, Value, json};

use crate::contract::{Contract, ProcedureContract, ProcedureKind, ServiceContract};
use crate::openapi::{
  Components, Dialect, access_value, by_tag, call_id_schema, canonical, exit_schema, frame_schema,
  prefix, pretty, procedure_base, schema_ref,
};
use crate::protocol::RPC_DISPATCH_PATH;
use crate::wire::{INITIAL_CREDITS, MAX_CURSOR_LEN, MAX_FRAME_BYTES, RpcFailure};

/// The AsyncAPI version [`document`] writes.
pub const ASYNCAPI_VERSION: &str = "3.0.0";

/// The WebSocket subprotocols a client offers (design note §7).
pub const SUBPROTOCOLS: [&str; 2] = ["idrpc.json.v1", "idrpc.postcard.v1"];

/// What [`document`] writes into `info` and `servers`, and the topics to list.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AsyncApiOptions {
  /// `info.title`.
  pub title: String,
  /// `info.version`.
  pub version: String,
  /// `info.description`.
  pub description: Option<String>,
  /// The RPC prefix (default [`RPC_DISPATCH_PATH`]); SSE channels are at `{path}/{tag}`.
  pub path: String,
  /// The WebSocket endpoint (default `{path}/ws`).
  pub ws_path: Option<String>,
  /// The servers' `host` (default `localhost`).
  pub host: String,
  /// `wss` and `https` (default) or `ws` and `http`.
  pub secure: bool,
  /// The channels' topics, listed in order.
  pub topics: Vec<ChannelTopic>,
}

impl Default for AsyncApiOptions {
  fn default() -> Self {
    Self::new("RPC", "1")
  }
}

impl AsyncApiOptions {
  /// A document titled `title` at API version `version`.
  pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
    Self {
      title: title.into(),
      version: version.into(),
      description: None,
      path: RPC_DISPATCH_PATH.to_owned(),
      ws_path: None,
      host: "localhost".to_owned(),
      secure: true,
      topics: Vec::new(),
    }
  }

  /// Set `info.description`.
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }

  /// The RPC prefix the server is mounted at.
  #[must_use]
  pub fn with_path(mut self, path: impl Into<String>) -> Self {
    self.path = path.into();
    self
  }

  /// The WebSocket endpoint's path.
  #[must_use]
  pub fn with_ws_path(mut self, path: impl Into<String>) -> Self {
    self.ws_path = Some(path.into());
    self
  }

  /// The servers' host (`api.example.com`, `localhost:8080`).
  #[must_use]
  pub fn with_host(mut self, host: impl Into<String>) -> Self {
    self.host = host.into();
    self
  }

  /// Describe plain `ws` and `http` servers (development) instead of `wss` and `https`.
  #[must_use]
  pub fn plain_transport(mut self) -> Self {
    self.secure = false;
    self
  }

  /// List `topic`.
  #[must_use]
  pub fn with_topic(mut self, topic: ChannelTopic) -> Self {
    self.topics.push(topic);
    self
  }
}

/// A topic of the `channels` module, joined through the subscription procedure `join` that
/// `Channels::serve_joins` registered (input `{"topic": <name>, "meta": …}`); subscribers receive
/// a `joined` snapshot, every `message` published on the topic, and `presence` diffs.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ChannelTopic {
  /// The topic name.
  pub name: String,
  /// The tag of the join procedure (`Chat.join`, say).
  pub join: String,
  /// Human-readable description.
  pub description: Option<String>,
  /// JSON Schema of one published message body.
  pub payload: Value,
}

impl ChannelTopic {
  /// Topic `name`, joined through procedure `join`, carrying message bodies of `payload` (a
  /// JSON Schema).
  pub fn new(name: impl Into<String>, join: impl Into<String>, payload: Value) -> Self {
    Self {
      name: name.into(),
      join: join.into(),
      description: None,
      payload,
    }
  }

  /// Set the description.
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }
}

/// The AsyncAPI 3.0 document of `contract`'s stream kinds and `options`' topics (see the
/// [module docs](self)).
pub fn document(contract: &Contract, options: &AsyncApiOptions) -> Value {
  let extra: Vec<&Value> = options.topics.iter().map(|t| &t.payload).collect();
  let mut c = Components::for_contract(Dialect::AsyncApi, contract, &extra);
  let prefix = prefix(&options.path);
  let ws_path = options
    .ws_path
    .clone()
    .unwrap_or_else(|| format!("{prefix}/ws"));
  let mut channels = Map::new();
  let mut operations = Map::new();
  connection_channel(&mut channels, &mut operations);
  for (tag, versions) in by_tag(contract) {
    for (service, procedure) in versions {
      if procedure.kind.is_unary() {
        continue;
      }
      let stream = Stream {
        tag: &tag,
        service,
        procedure,
        base: procedure_base(service, procedure),
      };
      stream.add(&mut c, &prefix, &mut channels, &mut operations);
    }
  }
  for topic in &options.topics {
    add_topic(&c, topic, &prefix, &mut channels, &mut operations);
  }
  let (ws, http) = if options.secure {
    ("wss", "https")
  } else {
    ("ws", "http")
  };
  let mut info = json!({ "title": options.title, "version": options.version });
  if let Some(description) = &options.description {
    info["description"] = json!(description);
  }
  let doc = json!({
    "asyncapi": ASYNCAPI_VERSION,
    "info": info,
    "defaultContentType": "application/json",
    "servers": {
      "ws": {
        "host": options.host,
        "protocol": ws,
        "pathname": ws_path,
        "description": format!(
          "Every procedure kind over one WebSocket connection, multiplexed by call id. The \
           client offers the subprotocols {}; frames are at most {MAX_FRAME_BYTES} bytes \
           (over: close 1009); the initial credit window is {INITIAL_CREDITS} items per call.",
          SUBPROTOCOLS.join(" and ")
        ),
      },
      "sse": {
        "host": options.host,
        "protocol": http,
        "description": "Server streams and subscriptions as server-sent events (`Accept: text/event-stream`); calls that change state go over `POST` next to it.",
      },
    },
    "channels": channels,
    "operations": operations,
    "components": { "schemas": c.into_schemas() },
  });
  canonical(doc)
}

/// [`document`] as pretty JSON: two-space indent, sorted keys, a trailing newline.
pub fn to_json(contract: &Contract, options: &AsyncApiOptions) -> String {
  pretty(&document(contract, options))
}

/// The WebSocket channel binding: the handshake's subprotocol header.
fn ws_binding() -> Value {
  json!({
    "ws": {
      "method": "GET",
      "headers": {
        "type": "object",
        "properties": {
          "Sec-WebSocket-Protocol": {
            "type": "string",
            "description": "The codecs the client offers; the server picks the first it knows, none known refuses the upgrade.",
            "enum": SUBPROTOCOLS,
          },
        },
      },
      "bindingVersion": "0.1.0",
    },
  })
}

fn ws_ref() -> Value {
  json!({ "$ref": "#/servers/ws" })
}

fn sse_ref() -> Value {
  json!({ "$ref": "#/servers/sse" })
}

fn message(name: &str, summary: &str, payload: Value) -> Value {
  json!({
    "name": name,
    "summary": summary,
    "contentType": "application/json",
    "payload": payload,
  })
}

fn message_refs(channel: &str, names: &[&str]) -> Vec<Value> {
  names
    .iter()
    .map(|m| json!({ "$ref": format!("#/channels/{channel}/messages/{m}") }))
    .collect()
}

fn operation(channel: &str, action: &str, summary: &str, messages: &[&str]) -> Value {
  json!({
    "action": action,
    "channel": { "$ref": format!("#/channels/{channel}") },
    "summary": summary,
    "messages": message_refs(channel, messages),
  })
}

fn cursor_schema(description: &str) -> Value {
  json!({
    "type": "string",
    "pattern": "^c1\\.[A-Za-z0-9_-]{12}$",
    "maxLength": MAX_CURSOR_LEN,
    "description": description,
  })
}

fn session_schema() -> Value {
  json!({
    "type": "string",
    "pattern": "^[A-Za-z0-9_-]{22}$",
    "description": "The resume session: 128 random bits, bound to the principal that opened it.",
  })
}

/// The frames of call id 0.
fn connection_channel(channels: &mut Map<String, Value>, operations: &mut Map<String, Value>) {
  let id0 = json!({ "const": 0 });
  let with_id0 = |mut frame: Value| {
    frame["properties"]["id"] = id0.clone();
    frame
  };
  let hello = with_id0(frame_schema(
    "hello",
    json!({
      "type": "object",
      "required": ["session", "codec", "max_frame", "window"],
      "properties": {
        "session": session_schema(),
        "codec": { "type": "string", "enum": SUBPROTOCOLS },
        "max_frame": { "type": "integer", "maximum": MAX_FRAME_BYTES },
        "window": { "type": "integer", "description": "The initial credit window per call." },
      },
    }),
    &[],
  ));
  let resume = with_id0(frame_schema(
    "resume",
    json!({
      "type": "object",
      "required": ["session", "calls"],
      "properties": {
        "session": session_schema(),
        "calls": {
          "type": "array",
          "items": {
            "type": "object",
            "required": ["id", "cursor"],
            "properties": {
              "id": call_id_schema(),
              "cursor": cursor_schema("The last cursor the client saw for that call."),
            },
          },
        },
      },
    }),
    &[],
  ));
  let mut messages = Map::new();
  messages.insert(
    "hello".to_owned(),
    message(
      "hello",
      "The server's choices for the connection: session, codec, limits.",
      hello,
    ),
  );
  messages.insert(
    "resume".to_owned(),
    message(
      "resume",
      "Resume the calls of a session from their cursors; an expired buffer answers `exit` `ResumeExpired` per call.",
      resume,
    ),
  );
  messages.insert(
    "ping".to_owned(),
    message(
      "ping",
      "Application heartbeat where the transport has none.",
      with_id0(frame_schema("ping", Value::Null, &[])),
    ),
  );
  messages.insert(
    "pong".to_owned(),
    message(
      "pong",
      "Heartbeat answer.",
      with_id0(frame_schema("pong", Value::Null, &[])),
    ),
  );
  channels.insert(
    "connection".to_owned(),
    json!({
      "address": null,
      "title": "The connection (call id 0)",
      "description": "Frames that belong to the connection rather than a call. WebSocket uses its own ping (20 s, two missed close the connection).",
      "servers": [ws_ref()],
      "messages": messages,
      "bindings": ws_binding(),
    }),
  );
  operations.insert(
    "connection.send".to_owned(),
    operation(
      "connection",
      "send",
      "Greet the connection; answer pings.",
      &["hello", "pong"],
    ),
  );
  operations.insert(
    "connection.receive".to_owned(),
    operation(
      "connection",
      "receive",
      "Resume a session; heartbeats.",
      &["resume", "ping"],
    ),
  );
}

/// One stream-kind procedure version.
struct Stream<'a> {
  tag: &'a str,
  service: &'a ServiceContract,
  procedure: &'a ProcedureContract,
  base: String,
}

impl Stream<'_> {
  fn add(
    &self,
    c: &mut Components,
    prefix: &str,
    channels: &mut Map<String, Value>,
    operations: &mut Map<String, Value>,
  ) {
    let p = self.procedure;
    let kind = p.kind;
    let error = schema_ref(&c.error_component(&self.base, p, &RpcFailure::RESERVED_TAGS));
    let input = c.import(&p.input);
    let output = c.import(&p.output);
    let exit_value = if kind == ProcedureKind::ClientStream {
      output.clone()
    } else {
      json!({ "type": "null" })
    };
    let exit = exit_schema(exit_value, error);
    let label = format!("`{}` version {}", self.tag, self.service.version);
    let sends_items = matches!(kind, ProcedureKind::ClientStream | ProcedureKind::Bidi);
    let receives_items = kind != ProcedureKind::ClientStream;
    let cursor = match kind {
      ProcedureKind::Subscription => {
        "Resume position: an event-log position. Present on resumable subscriptions."
      }
      _ => "Resume position: the per-session message sequence, on resumable calls.",
    };

    // --- WebSocket ---
    let ws = format!("{}.ws", self.base);
    let mut call_extra = vec![
      ("tag", json!({ "const": self.tag }), true),
      ("v", json!({ "const": self.service.version }), false),
    ];
    if kind == ProcedureKind::Subscription {
      call_extra.push((
        "cursor",
        cursor_schema("Start after this cursor (the last one the client saw)."),
        false,
      ));
    }
    let call_payload = if sends_items {
      Value::Null
    } else {
      input.clone()
    };
    let mut messages = Map::new();
    let mut received = vec!["call", "cancel"];
    let mut sent = vec!["exit"];
    messages.insert(
      "call".to_owned(),
      message(
        &format!("{}.call", self.tag),
        &format!("Open a call of {label}."),
        frame_schema("call", call_payload, &call_extra),
      ),
    );
    messages.insert(
      "cancel".to_owned(),
      message(
        &format!("{}.cancel", self.tag),
        "Interrupt the call (`Cancelled`); its finalizers run.",
        frame_schema("cancel", Value::Null, &[]),
      ),
    );
    if sends_items {
      messages.insert(
        "clientItem".to_owned(),
        message(
          &format!("{}.clientItem", self.tag),
          "One input item.",
          frame_schema("item", input.clone(), &[]),
        ),
      );
      messages.insert(
        "end".to_owned(),
        message(
          &format!("{}.end", self.tag),
          "The client's last item was sent (half-close, flag `END`).",
          frame_schema("end", Value::Null, &[]),
        ),
      );
      messages.insert(
        "serverCredit".to_owned(),
        message(
          &format!("{}.serverCredit", self.tag),
          "The server accepts `credits` more input items.",
          frame_schema(
            "credit",
            Value::Null,
            &[("credits", json!({ "type": "integer", "minimum": 0 }), true)],
          ),
        ),
      );
      received.extend(["clientItem", "end"]);
      sent.push("serverCredit");
    }
    if receives_items {
      messages.insert(
        "serverItem".to_owned(),
        message(
          &format!("{}.serverItem", self.tag),
          "One output item.",
          frame_schema(
            "item",
            output.clone(),
            &[("cursor", cursor_schema(cursor), false)],
          ),
        ),
      );
      messages.insert(
        "clientCredit".to_owned(),
        message(
          &format!("{}.clientCredit", self.tag),
          "The client accepts `credits` more output items (a slow client slows its own stream).",
          frame_schema(
            "credit",
            Value::Null,
            &[("credits", json!({ "type": "integer", "minimum": 0 }), true)],
          ),
        ),
      );
      received.push("clientCredit");
      sent.insert(0, "serverItem");
    }
    messages.insert(
      "exit".to_owned(),
      message(
        &format!("{}.exit", self.tag),
        "The call's final `WireExit`; closes the call.",
        frame_schema("exit", exit.clone(), &[]),
      ),
    );
    let mut channel = self.channel(json!(self.tag), json!([ws_ref()]), messages);
    channel["bindings"] = ws_binding();
    channels.insert(ws.clone(), channel);
    operations.insert(
      format!("{ws}.receive"),
      self.operation(
        &ws,
        "receive",
        &format!("What a client sends on a call of {label}."),
        &received,
      ),
    );
    operations.insert(
      format!("{ws}.send"),
      self.operation(
        &ws,
        "send",
        &format!("What the server answers on a call of {label}."),
        &sent,
      ),
    );

    // --- Server-sent events ---
    if !matches!(
      kind,
      ProcedureKind::ServerStream | ProcedureKind::Subscription
    ) {
      return;
    }
    let sse = format!("{}.sse", self.base);
    let id = if kind == ProcedureKind::Subscription {
      cursor_schema("The event's cursor; reconnect with `Last-Event-ID` to resume after it.")
    } else {
      cursor_schema(cursor)
    };
    let mut messages = Map::new();
    let mut item = message(
      &format!("{}.event.item", self.tag),
      "`event: item`, `data:` one output item.",
      output,
    );
    item["headers"] = json!({
      "type": "object",
      "properties": { "event": { "const": "item" }, "id": id },
      "required": ["event"],
    });
    messages.insert("item".to_owned(), item);
    let mut end = message(
      &format!("{}.event.exit", self.tag),
      "`event: exit`, `data:` the call's `WireExit`; the stream ends.",
      exit,
    );
    end["headers"] = json!({
      "type": "object",
      "properties": { "event": { "const": "exit" } },
      "required": ["event"],
    });
    messages.insert("exit".to_owned(), end);
    channels.insert(
      sse.clone(),
      self.channel(
        json!(format!("{prefix}/{}", self.tag)),
        json!([sse_ref()]),
        messages,
      ),
    );
    let mut op = self.operation(
      &sse,
      "send",
      &format!(
        "`GET {prefix}/{}?input=<JSON>` with `Accept: text/event-stream`{}.",
        self.tag,
        if kind == ProcedureKind::Subscription {
          "; `Last-Event-ID` resumes after that cursor"
        } else {
          ""
        }
      ),
      &["item", "exit"],
    );
    op["bindings"] = json!({
      "http": {
        "method": "GET",
        "query": {
          "type": "object",
          "properties": {
            "input": { "type": "string", "description": "The input as URL-encoded JSON; absent means `null`." },
          },
        },
        "bindingVersion": "0.3.0",
      },
    });
    operations.insert(format!("{sse}.send"), op);
  }

  fn channel(&self, address: Value, servers: Value, messages: Map<String, Value>) -> Value {
    let p = self.procedure;
    let mut channel = json!({
      "address": address,
      "title": format!("{} ({}, version {})", self.tag, p.kind, self.service.version),
      "servers": servers,
      "messages": messages,
      "x-rpc-tag": self.tag,
      "x-rpc-kind": p.kind.as_str(),
      "x-rpc-version": self.service.version,
      "x-rpc-access": access_value(&p.access),
    });
    if let Some(description) = &p.description {
      channel["description"] = json!(description);
    }
    if let Some(items) = p.max_items {
      channel["x-rpc-max-items"] = json!(items);
    }
    if let Some(bytes) = p.max_bytes {
      channel["x-rpc-max-bytes"] = json!(bytes);
    }
    channel
  }

  fn operation(&self, channel: &str, action: &str, summary: &str, messages: &[&str]) -> Value {
    let mut op = operation(channel, action, summary, messages);
    if self.procedure.deprecated {
      op["description"] = json!("Deprecated: still served; generated clients warn.");
    }
    op
  }
}

fn add_topic(
  c: &Components,
  topic: &ChannelTopic,
  prefix: &str,
  channels: &mut Map<String, Value>,
  operations: &mut Map<String, Value>,
) {
  let name = crate::openapi::ident(&topic.name);
  let body = c.import(&topic.payload);
  // `channels::ChannelEvent` items (`{"type": …}`) and `channels::Presence` entries.
  let member = json!({
    "type": "object",
    "properties": {
      "key": { "type": "string", "description": "The principal's id." },
      "conn": { "type": "string", "description": "The joined connection." },
      "node": { "type": "string" },
      "meta": { "description": "The metadata the connection joined with." },
    },
    "required": ["key", "conn", "node", "meta"],
  });
  let members = json!({ "type": "array", "items": member });
  let topic_name = json!({ "const": topic.name });
  let events = [
    (
      "joined",
      "The first item: this connection's entry and every member.",
      json!({
        "type": "object",
        "properties": {
          "type": { "const": "joined" }, "topic": topic_name, "me": member, "members": members,
        },
        "required": ["type", "topic", "me", "members"],
      }),
    ),
    (
      "message",
      "One message published on the topic, on any node.",
      json!({
        "type": "object",
        "properties": {
          "type": { "const": "message" }, "topic": topic_name, "node": { "type": "string" },
          "from": { "type": ["string", "null"] }, "body": body,
        },
        "required": ["type", "topic", "node", "body"],
      }),
    ),
    (
      "presence",
      "Connections that joined and left since the last diff.",
      json!({
        "type": "object",
        "properties": {
          "type": { "const": "presence" }, "topic": topic_name, "joins": members,
          "leaves": members,
        },
        "required": ["type", "topic", "joins", "leaves"],
      }),
    ),
  ];
  let names: Vec<&str> = events.iter().map(|(n, _, _)| *n).collect();
  // A join is a call of the subscription procedure `join` with the topic as its input.
  for (transport, server, address) in [
    ("ws", ws_ref(), json!(topic.join)),
    ("sse", sse_ref(), json!(format!("{prefix}/{}", topic.join))),
  ] {
    let key = format!("topic.{name}.{transport}");
    let mut messages = Map::new();
    for (event, summary, payload) in &events {
      messages.insert(
        (*event).to_owned(),
        message(&format!("{}.{event}", topic.name), summary, payload.clone()),
      );
    }
    let mut channel = json!({
      "address": address,
      "title": format!("Topic {}", topic.name),
      "servers": [server],
      "messages": messages,
      "x-rpc-tag": topic.join,
      "x-rpc-input": { "topic": topic.name },
      "x-rpc-join": "The topic's JoinPolicy decides every join and resume (deny by default).",
    });
    if let Some(description) = &topic.description {
      channel["description"] = json!(description);
    }
    if transport == "ws" {
      channel["bindings"] = ws_binding();
    }
    channels.insert(key.clone(), channel);
    operations.insert(
      format!("{key}.send"),
      operation(
        &key,
        "send",
        &format!("Deliver what is published on `{}`.", topic.name),
        &names,
      ),
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn options_set_every_member() {
    let o = AsyncApiOptions::new("T", "2")
      .with_description("d")
      .with_path("/api")
      .with_ws_path("/socket")
      .with_host("example.com")
      .plain_transport()
      .with_topic(
        ChannelTopic::new("room", "Chat.join", json!({"type": "string"})).with_description("r"),
      );
    assert_eq!(o.ws_path.as_deref(), Some("/socket"));
    assert!(!o.secure);
    assert_eq!(o.topics[0].description.as_deref(), Some("r"));
    assert_eq!(o.topics[0].join, "Chat.join");
    assert_eq!(AsyncApiOptions::default().title, "RPC");
    let doc = document(&Contract::new(), &o);
    assert_eq!(doc["servers"]["ws"]["protocol"], "ws");
    assert_eq!(doc["servers"]["ws"]["pathname"], "/socket");
    assert_eq!(doc["info"]["description"], "d");
    assert_eq!(
      doc["channels"]["topic.room.sse"]["address"],
      "/api/Chat.join"
    );
    let ws = &doc["channels"]["topic.room.ws"];
    assert_eq!(ws["address"], "Chat.join");
    assert_eq!(ws["x-rpc-input"], json!({"topic": "room"}));
    let presence = &ws["messages"]["presence"]["payload"];
    assert_eq!(presence["properties"]["joins"]["type"], "array");
    assert_eq!(presence["properties"]["type"]["const"], "presence");
  }
}
