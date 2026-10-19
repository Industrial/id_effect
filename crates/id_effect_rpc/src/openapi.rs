//! OpenAPI 3.1 from the service contract (web.rpc #7), and the routes that serve the contract
//! documents.
//!
//! [`document`] reads a [`Contract`] (`Service::contract()`, `RpcService::document()`, or
//! `rpc.contract.json`) and describes the plain-HTTP side of the 0.12 wire (design note
//! `r12-rpc-design.md` §2–§4):
//!
//! | Kind | Operations at `{path}/{tag}` |
//! |---|---|
//! | query | `GET ?input=<JSON>` (ETag, `If-None-Match` → 304) and `POST` with the input as body |
//! | mutation | `POST`, the optional `Idempotency-Key` header |
//! | server stream | `POST`, answered with `application/x-ndjson` frames (`item`…, then `exit`) |
//! | client stream | `POST` with `application/x-ndjson` items, answered with a `WireResult` |
//! | two-way stream, subscription | none: [`crate::asyncapi`] describes them (WebSocket, SSE) |
//!
//! `POST {path}` is the envelope route: one `WireCall`, or a batch (a JSON array of at most
//! [`MAX_BATCH_CALLS`] calls) answered in request order. Its body lists every
//! `(tag, version)` the contract serves, so older service versions are described there too;
//! the per-tag path describes the **newest** version of its procedure (what a call without
//! `x-rpc-version` gets), and its `x-rpc-version` parameter lists every version served.
//!
//! Each operation lists the `x-rpc: 1` header (every `POST`), `x-rpc-deadline-ms`,
//! `x-rpc-version`, `x-correlation-id`, the responses that carry a `WireResult` (200, the typed
//! error's status — 400 unless the error type declares another — and the reserved failures:
//! 422 `InvalidInput`, 429 `ResourceExhausted`, 500 `InternalError`, 504 `DeadlineExceeded`)
//! and the RFC 9457 Problem responses of the transport (400, 401/403 for guarded procedures,
//! 403 without `x-rpc`, 404, 409 for an `Idempotency-Key` conflict, 413, 415, 429).
//! Extensions carry what OpenAPI has no field for: `x-rpc-tag`, `x-rpc-kind`, `x-rpc-version`,
//! `x-rpc-access` (`{"public":{}}` or `{"guarded":{"policies":[…]}}`), `x-rpc-idempotent`,
//! `x-rpc-max-items`, `x-rpc-max-bytes`.
//!
//! # Error unions
//!
//! A procedure's failure is `{"_tag": "Failure", "error": …}` where `error` is the component
//! `<Service>-v<N>-<procedure>-Error`: a `oneOf` of its typed variants and the reserved
//! failures, with `discriminator: {"propertyName": "_tag", "mapping": {…}}`. Each variant is a
//! component `error-<Tag>`; when two variants with the same tag differ (two error types both
//! declaring `NotFound` with other fields), each is named `error-<Tag>-<hash>` instead, the
//! hash being the first 8 hex digits of the SHA-256 of the tag and the variant's JSON.
//!
//! # Component names
//!
//! Contract schemas are JSON Schema 2020-12 documents (the dialect OpenAPI 3.1 uses, declared
//! by `jsonSchemaDialect`) whose `$defs` are keyed by Rust type paths. Their `$defs` move into
//! `components/schemas` and every `#/$defs/…` reference is rewritten. A definition is named after
//! the last `::` segment of its key (`my_app::todo::Todo` → `Todo`; characters other than ASCII
//! letters, digits and `_` become `_`, trailing `_` are trimmed). When two keys share that
//! segment, both take the shortest `::` suffix, joined with `.`, that no other key shares
//! (`a::Item`, `b::Item` → `a.Item`, `b.Item`); the names depend only on the set of keys, not on
//! the order they were seen. The same key with two different bodies (contracts merged from two
//! builds) keeps the name for the body whose JSON text sorts first and numbers the others `_2`,
//! `_3`, … in that order. Names this module generates contain a `-` (`rpc-Problem`,
//! `Todos-v1-add-Result`, `error-NotFound`), which a definition name never does, so the two never
//! collide. Rust type paths are not stable across compiler versions; a checked-in document
//! changes when they do.
//!
//! Output is deterministic: object keys sorted, services by `(name, version)`, procedures by
//! name, lists in contract order. [`to_json`] adds two-space indentation and a trailing newline.
//!
//! ```rust
//! use id_effect_rpc::contract::Contract;
//! use id_effect_rpc::openapi::{OpenApiOptions, document};
//!
//! let contract = Contract::from_json(include_str!("../rpc.contract.json")).unwrap();
//! let doc = document(&contract, &OpenApiOptions::new("Todos API", "1.0.0"));
//! assert_eq!(doc["openapi"], "3.1.0");
//! assert!(doc["paths"]["/rpc/Todos.get"]["get"].is_object());
//! assert!(doc["paths"]["/rpc/Todos.add"].get("get").is_none());
//! assert_eq!(
//!   doc["components"]["schemas"]["Todos-v1-add-Error"]["discriminator"]["propertyName"],
//!   "_tag"
//! );
//! ```
//!
//! # Serving (ADR 0013 R7)
//!
//! [`ContractDocs::router`] serves `{path}/openapi.json`, `{path}/asyncapi.json` and
//! `{path}/_contract`. They list every procedure, so outside the `dev` and `test` profiles
//! ([`ContractDocs::profile`], default the `ID_EFFECT_PROFILE` variable, else `dev`) **no route
//! is mounted** unless a [`guard`](ContractDocs::guard) decides first or the documents are made
//! public with [`ContractDocs::public_dangerous`] (recorded in the posture registry). Unmounted,
//! the paths fall through to the `/rpc/{tag}` route, which answers 404 (no such procedure).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::asyncapi::{self, AsyncApiOptions};
use crate::cancel::DEADLINE_HEADER;
use crate::codec::NDJSON;
use crate::contract::{
  Contract, ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract,
};
use crate::idempotency::{
  IDEMPOTENCY_KEY_HEADER, MAX_KEY_LEN, MIN_ANONYMOUS_KEY_LEN, REPLAYED_HEADER,
};
use crate::protocol::RPC_DISPATCH_PATH;
use crate::registry::RouteGuard;
use crate::server::{RPC_HEADER, VERSION_HEADER};
use crate::wire::{MAX_BATCH_CALLS, MAX_CALL_ID, RpcFailure};

/// The OpenAPI version [`document`] writes.
pub const OPENAPI_VERSION: &str = "3.1.0";

/// The schema dialect of every contract schema (`jsonSchemaDialect`).
pub const JSON_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The correlation header every response echoes (the `id` of `InternalError`).
pub const CORRELATION_HEADER: &str = "x-correlation-id";

/// RFC 9457 Problem Details media type.
const PROBLEM_JSON: &str = "application/problem+json";

// The header that runs a batch sequentially (design note §2.2; r12-e12-1-batching).
use crate::batch::ORDERED_HEADER;

/// What [`document`] writes into `info`, `servers` and the paths.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenApiOptions {
  /// `info.title`.
  pub title: String,
  /// `info.version` (the API's version string, not a service contract version).
  pub version: String,
  /// `info.description`.
  pub description: Option<String>,
  /// The RPC prefix the server is mounted at (default [`RPC_DISPATCH_PATH`]).
  pub path: String,
  /// `servers[].url`, in order (none by default: paths are relative to the document).
  pub servers: Vec<String>,
}

impl Default for OpenApiOptions {
  fn default() -> Self {
    Self::new("RPC", "1")
  }
}

impl OpenApiOptions {
  /// A document titled `title` at API version `version`, for a server at [`RPC_DISPATCH_PATH`].
  pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
    Self {
      title: title.into(),
      version: version.into(),
      description: None,
      path: RPC_DISPATCH_PATH.to_owned(),
      servers: Vec::new(),
    }
  }

  /// Set `info.description`.
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }

  /// The prefix `RpcServer::with_path` mounted the routes at.
  #[must_use]
  pub fn with_path(mut self, path: impl Into<String>) -> Self {
    self.path = path.into();
    self
  }

  /// Add a `servers` entry.
  #[must_use]
  pub fn with_server(mut self, url: impl Into<String>) -> Self {
    self.servers.push(url.into());
    self
  }
}

/// The OpenAPI 3.1 document of `contract` (see the [module docs](self)).
pub fn document(contract: &Contract, options: &OpenApiOptions) -> Value {
  let mut c = Components::for_contract(Dialect::OpenApi, contract, &[]);
  c.insert("rpc-Problem", problem_schema());
  c.insert("rpc-CallId", call_id_schema());
  let prefix = prefix(&options.path);
  let mut paths = Map::new();
  let mut calls = Vec::new();
  let mut results = Vec::new();
  for (tag, versions) in by_tag(contract) {
    let served: Vec<u32> = versions
      .iter()
      .map(|(service, _)| service.version)
      .collect();
    for (i, (service, procedure)) in versions.iter().enumerate() {
      if procedure.kind.is_unary() {
        let newest = i + 1 == versions.len();
        let (call, result) = unary_components(&mut c, service, procedure, &tag, newest);
        calls.push(schema_ref(&call));
        results.push(schema_ref(&result));
      }
    }
    let Some((service, procedure)) = versions.last() else {
      continue;
    };
    let op = Op {
      tag: &tag,
      service,
      procedure,
      served: &served,
    };
    if let Some(item) = op.path_item(&mut c) {
      paths.insert(format!("{prefix}/{tag}"), item);
    }
  }
  if !calls.is_empty() {
    c.insert("rpc-Call", json!({ "anyOf": calls }));
    c.insert("rpc-Result", json!({ "anyOf": results }));
    let envelope = if prefix.is_empty() {
      "/".to_owned()
    } else {
      prefix.clone()
    };
    paths.insert(envelope, envelope_path_item());
  }
  let mut info = json!({ "title": options.title, "version": options.version });
  if let Some(description) = &options.description {
    info["description"] = json!(description);
  }
  let mut doc = json!({
    "openapi": OPENAPI_VERSION,
    "jsonSchemaDialect": JSON_SCHEMA_DIALECT,
    "info": info,
    "tags": service_tags(contract),
    "paths": paths,
    "components": { "schemas": c.into_schemas() },
  });
  if !options.servers.is_empty() {
    let servers: Vec<Value> = options
      .servers
      .iter()
      .map(|url| json!({ "url": url }))
      .collect();
    doc["servers"] = Value::Array(servers);
  }
  canonical(doc)
}

/// [`document`] as pretty JSON: two-space indent, sorted keys, a trailing newline.
pub fn to_json(contract: &Contract, options: &OpenApiOptions) -> String {
  pretty(&document(contract, options))
}

/// [`document`] as YAML.
pub fn to_yaml(contract: &Contract, options: &OpenApiOptions) -> Result<String, serde_yaml::Error> {
  serde_yaml::to_string(&document(contract, options))
}

// ---------------------------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------------------------

/// The `-Error`, `-Result` and `-Call` components of one unary procedure version; returns the
/// call and result names.
fn unary_components(
  c: &mut Components,
  service: &ServiceContract,
  procedure: &ProcedureContract,
  tag: &str,
  newest: bool,
) -> (String, String) {
  let base = procedure_base(service, procedure);
  let result = result_component(c, &base, service, procedure, tag);
  let input = c.import(&procedure.input);
  let call = format!("{base}-Call");
  let mut required = vec!["tag", "payload"];
  if !newest {
    required.push("v");
  }
  c.insert(
    &call,
    json!({
      "type": "object",
      "description": format!(
        "A `WireCall` of `{tag}` version {}{}.",
        service.version,
        if newest { " (the default when `v` is absent)" } else { "" }
      ),
      "required": required,
      "properties": {
        "id": schema_ref("rpc-CallId"),
        "tag": { "const": tag },
        "v": { "const": service.version },
        "payload": input,
      },
    }),
  );
  (call, result)
}

/// `<base>-Result` (and `<base>-Error`): the `WireResult` of a call answered with one output.
fn result_component(
  c: &mut Components,
  base: &str,
  service: &ServiceContract,
  procedure: &ProcedureContract,
  tag: &str,
) -> String {
  let error = c.error_component(base, procedure, &RESERVED_UNARY);
  let output = c.import(&procedure.output);
  let result = format!("{base}-Result");
  c.insert(
    &result,
    json!({
      "type": "object",
      "description": format!("The `WireResult` of `{tag}` version {}.", service.version),
      "required": ["tag", "exit"],
      "properties": {
        "id": schema_ref("rpc-CallId"),
        "tag": { "const": tag },
        "exit": exit_schema(output, schema_ref(&error)),
      },
    }),
  );
  result
}

/// One procedure's operations at `{path}/{tag}`.
struct Op<'a> {
  tag: &'a str,
  service: &'a ServiceContract,
  procedure: &'a ProcedureContract,
  served: &'a [u32],
}

impl Op<'_> {
  fn path_item(&self, c: &mut Components) -> Option<Value> {
    let base = procedure_base(self.service, self.procedure);
    let mut item = Map::new();
    match self.procedure.kind {
      ProcedureKind::Query => {
        item.insert("get".to_owned(), self.unary(c, &base, true));
        item.insert("post".to_owned(), self.unary(c, &base, false));
      }
      ProcedureKind::Mutation => {
        item.insert("post".to_owned(), self.unary(c, &base, false));
      }
      ProcedureKind::ServerStream => {
        item.insert("post".to_owned(), self.server_stream(c, &base));
      }
      ProcedureKind::ClientStream => {
        item.insert("post".to_owned(), self.client_stream(c, &base));
      }
      ProcedureKind::Bidi | ProcedureKind::Subscription => return None,
    }
    Some(Value::Object(item))
  }

  /// The members every operation has.
  fn operation(&self, method: &str, post: bool) -> Map<String, Value> {
    let p = self.procedure;
    let mut op = Map::new();
    op.insert(
      "operationId".to_owned(),
      json!(format!("{}_{method}", self.tag)),
    );
    op.insert("tags".to_owned(), json!([self.service.name]));
    let summary = p
      .description
      .as_deref()
      .and_then(|d| d.lines().next())
      .filter(|line| !line.trim().is_empty())
      .unwrap_or(self.tag);
    op.insert("summary".to_owned(), json!(summary));
    let mut description = format!(
      "`{}` ({}), service `{}` version {}.",
      self.tag, p.kind, self.service.name, self.service.version
    );
    if let Some(text) = &p.description {
      description = format!("{text}\n\n{description}");
    }
    op.insert("description".to_owned(), json!(description));
    if p.deprecated {
      op.insert("deprecated".to_owned(), json!(true));
    }
    op.insert("x-rpc-tag".to_owned(), json!(self.tag));
    op.insert("x-rpc-kind".to_owned(), json!(p.kind.as_str()));
    op.insert("x-rpc-version".to_owned(), json!(self.service.version));
    op.insert("x-rpc-access".to_owned(), access_value(&p.access));
    if p.idempotent {
      op.insert("x-rpc-idempotent".to_owned(), json!(true));
    }
    let mut params = Vec::new();
    if post {
      params.push(rpc_header_param());
    }
    params.extend([self.version_param(), deadline_param(), correlation_param()]);
    op.insert("parameters".to_owned(), Value::Array(params));
    op
  }

  fn version_param(&self) -> Value {
    let newest = self.service.version;
    json!({
      "name": VERSION_HEADER,
      "in": "header",
      "required": false,
      "description": format!(
        "The service version to call (default: the newest serving `{}`, {newest}). This \
         operation describes version {newest}; the body of `POST` on the envelope route \
         describes every version.",
        self.tag
      ),
      "schema": { "type": "integer", "enum": self.served, "default": newest },
    })
  }

  fn params(op: &mut Map<String, Value>) -> &mut Vec<Value> {
    match op.get_mut("parameters") {
      Some(Value::Array(params)) => params,
      _ => unreachable!("operation() sets parameters"),
    }
  }

  fn unary(&self, c: &mut Components, base: &str, get: bool) -> Value {
    let p = self.procedure;
    let mut op = self.operation(if get { "GET" } else { "POST" }, !get);
    let input = c.import(&p.input);
    if get {
      Self::params(&mut op).extend([
        json!({
          "name": "input",
          "in": "query",
          "required": !accepts_null(&input),
          "description": "The input as JSON (URL-encoded); absent means `null`.",
          "content": { "application/json": { "schema": input.clone() } },
        }),
        json!({
          "name": "If-None-Match",
          "in": "header",
          "required": false,
          "description": "The ETag of an earlier success: a match answers 304.",
          "schema": { "type": "string" },
        }),
      ]);
    } else {
      op.insert("requestBody".to_owned(), json_body(input));
    }
    if p.kind == ProcedureKind::Mutation {
      Self::params(&mut op).push(idempotency_param(p.access.is_public()));
    }
    let result = schema_ref(&format!("{base}-Result"));
    let mut responses = self.wire_responses(Some(&result), !get);
    let correlation = header_doc("The call's correlation id.");
    if get {
      responses.insert(
        "200".to_owned(),
        json!({
          "description": "Success; its `ETag` validates it.",
          "headers": {
            "ETag": header_doc("A strong validator of this body."),
            "Cache-Control": header_doc("`private, no-cache`."),
            CORRELATION_HEADER: correlation,
          },
          "content": { "application/json": { "schema": result } },
        }),
      );
      responses.insert(
        "304".to_owned(),
        json!({ "description": "Not modified: the `If-None-Match` ETag is current." }),
      );
    } else if p.kind == ProcedureKind::Mutation {
      responses.insert(
        "200".to_owned(),
        json!({
          "description": "Success, or the replay of a stored result.",
          "headers": {
            REPLAYED_HEADER: header_doc("`true` when the result is a replay."),
            CORRELATION_HEADER: correlation,
          },
          "content": { "application/json": { "schema": result } },
        }),
      );
      responses.insert(
        "409".to_owned(),
        problem_response("The `Idempotency-Key` was used with another payload, or is in progress."),
      );
    }
    op.insert("responses".to_owned(), Value::Object(responses));
    Value::Object(op)
  }

  fn server_stream(&self, c: &mut Components, base: &str) -> Value {
    let p = self.procedure;
    let mut op = self.operation("POST", true);
    let input = c.import(&p.input);
    let output = c.import(&p.output);
    let error = c.error_component(base, p, &RESERVED_UNARY);
    let frame = format!("{base}-Frame");
    c.insert(
      &frame,
      json!({
        "description": "One NDJSON line: an `item` frame per output, then one `exit` frame.",
        "oneOf": [
          frame_schema("item", output, &[]),
          frame_schema("exit", exit_schema(json!({ "type": "null" }), schema_ref(&error)), &[]),
        ],
      }),
    );
    op.insert("requestBody".to_owned(), json_body(input));
    let mut responses = self.wire_responses(None, true);
    responses.insert(
      "200".to_owned(),
      json!({
        "description": format!(
          "The stream: `{NDJSON}`, one frame per line. A failure, before or after the first \
           item, is the `exit` frame's `Failure`; a client disconnect interrupts the handler."
        ),
        "headers": { CORRELATION_HEADER: header_doc("The call's correlation id.") },
        "content": { NDJSON: { "schema": schema_ref(&frame) } },
      }),
    );
    op.insert("responses".to_owned(), Value::Object(responses));
    Value::Object(op)
  }

  fn client_stream(&self, c: &mut Components, base: &str) -> Value {
    let p = self.procedure;
    let mut op = self.operation("POST", true);
    let input = c.import(&p.input);
    let mut caps = Vec::new();
    if let Some(items) = p.max_items {
      caps.push(format!("at most {items} items"));
      op.insert("x-rpc-max-items".to_owned(), json!(items));
    }
    if let Some(bytes) = p.max_bytes {
      caps.push(format!("at most {bytes} bytes"));
      op.insert("x-rpc-max-bytes".to_owned(), json!(bytes));
    }
    caps.push("64 KiB per line".to_owned());
    op.insert(
      "requestBody".to_owned(),
      json!({
        "required": true,
        "description": format!(
          "The input items, one JSON value per line, chunked: {}. Over a cap is the \
           `ResourceExhausted` failure.",
          caps.join(", ")
        ),
        "content": { NDJSON: { "schema": input } },
      }),
    );
    let result = result_component(c, base, self.service, p, self.tag);
    let responses = self.wire_responses(Some(&schema_ref(&result)), true);
    op.insert("responses".to_owned(), Value::Object(responses));
    Value::Object(op)
  }

  /// The `WireResult` responses (when the call answers one) and the Problem responses.
  fn wire_responses(&self, result: Option<&Value>, post: bool) -> Map<String, Value> {
    let p = self.procedure;
    let mut r = Map::new();
    let malformed = "Problem: a malformed input, envelope, `x-rpc-version` or `Idempotency-Key`.";
    match result {
      Some(result) => {
        let wire = |description: &str, problem: bool| {
          let mut content = Map::new();
          content.insert("application/json".to_owned(), json!({ "schema": result }));
          if problem {
            content.insert(
              PROBLEM_JSON.to_owned(),
              json!({ "schema": schema_ref("rpc-Problem") }),
            );
          }
          json!({ "description": description, "content": content })
        };
        r.insert("200".to_owned(), wire("Success.", false));
        let typed = if p.error.is_some() {
          "A typed error of the procedure (the status its type declares; 400 by default)"
        } else {
          "The procedure declares no typed error"
        };
        r.insert(
          "400".to_owned(),
          wire(&format!("{typed}. {malformed}"), true),
        );
        r.insert(
          "422".to_owned(),
          wire("`InvalidInput`: the input failed Schema decode.", false),
        );
        r.insert(
          "429".to_owned(),
          wire(
            "`ResourceExhausted`: a per-call limit. Problem: the rate limit.",
            true,
          ),
        );
        r.insert(
          "500".to_owned(),
          wire("`InternalError`: a defect; only the correlation id.", false),
        );
        r.insert(
          "504".to_owned(),
          wire("`DeadlineExceeded`: the capped deadline expired.", false),
        );
      }
      None => {
        r.insert("400".to_owned(), problem_response(malformed));
        r.insert("429".to_owned(), problem_response("The rate limit."));
      }
    }
    if post {
      let refused = if p.access.is_public() {
        "The `x-rpc: 1` header is missing."
      } else {
        "The `x-rpc: 1` header is missing, or a guard refused the caller."
      };
      r.insert("403".to_owned(), problem_response(refused));
      r.insert(
        "413".to_owned(),
        problem_response("The request body is over the server's cap."),
      );
      r.insert(
        "415".to_owned(),
        problem_response("The Content-Type is not a codec the server speaks."),
      );
    } else if !p.access.is_public() {
      r.insert(
        "403".to_owned(),
        problem_response("A guard refused the caller."),
      );
    }
    if !p.access.is_public() {
      r.insert(
        "401".to_owned(),
        problem_response("A guard needs an authenticated caller."),
      );
    }
    r.insert(
      "404".to_owned(),
      problem_response("No such procedure at the requested version."),
    );
    r
  }
}

fn json_body(schema: Value) -> Value {
  json!({
    "required": true,
    "description": "The input.",
    "content": { "application/json": { "schema": schema } },
  })
}

fn idempotency_param(public: bool) -> Value {
  let min = if public { MIN_ANONYMOUS_KEY_LEN } else { 1 };
  let anonymous = if public {
    format!(
      " Anonymous callers are scoped by client IP and need keys of at least \
       {MIN_ANONYMOUS_KEY_LEN} characters (400 Problem `IdempotencyKeyTooShort`)."
    )
  } else {
    String::new()
  };
  json!({
    "name": IDEMPOTENCY_KEY_HEADER,
    "in": "header",
    "required": false,
    "description": format!(
      "Replays the stored `WireResult` of an earlier call with the same key (24 h). The same \
       key with another payload, or while that call runs, is a 409 Problem.{anonymous}"
    ),
    "schema": {
      "type": "string",
      "minLength": min,
      "maxLength": MAX_KEY_LEN,
      "pattern": "^[\\x21-\\x7E]+$",
    },
  })
}

fn envelope_path_item() -> Value {
  let call = schema_ref("rpc-Call");
  let result = schema_ref("rpc-Result");
  let problem = schema_ref("rpc-Problem");
  let wire = |description: &str| json!({ "description": description, "content": { "application/json": { "schema": result } } });
  json!({
    "post": {
      "operationId": "rpc_envelope",
      "summary": "One call envelope, or a batch",
      "description": format!(
        "A `WireCall` answered with its `WireResult` (the exit's status), or a batch: a JSON \
         array of at most {MAX_BATCH_CALLS} calls with distinct ids, answered 200 with one \
         `WireResult` per call in request order, each failing alone. Only queries and mutations \
         may be called here or batched. `{ORDERED_HEADER}: true` runs a batch sequentially; a \
         batch is charged one rate-limit token per call."
      ),
      "parameters": [
        rpc_header_param(),
        deadline_param(),
        correlation_param(),
        {
          "name": ORDERED_HEADER,
          "in": "header",
          "required": false,
          "description": "Batches only: run the calls one after the other in array order.",
          "schema": { "type": "boolean" },
        },
        idempotency_param(false),
      ],
      "requestBody": {
        "required": true,
        "content": {
          "application/json": {
            "schema": {
              "oneOf": [
                call,
                { "type": "array", "minItems": 1, "maxItems": MAX_BATCH_CALLS, "items": call },
              ],
            },
          },
        },
      },
      "responses": {
        "200": {
          "description": "Success of a single call, or every result of a batch.",
          "content": {
            "application/json": {
              "schema": { "oneOf": [result, { "type": "array", "items": result }] },
            },
          },
        },
        "400": {
          "description": "A typed error of a single call. Problem: a malformed envelope or batch.",
          "content": {
            "application/json": { "schema": result },
            PROBLEM_JSON: { "schema": problem },
          },
        },
        "403": problem_response("The `x-rpc: 1` header is missing, or a guard refused the caller."),
        "404": problem_response("No such procedure at the requested version."),
        "409": problem_response("The `Idempotency-Key` was used with another payload, or is in progress."),
        "413": problem_response("The body or the batch is over the server's cap."),
        "415": problem_response("The Content-Type is not a codec the server speaks."),
        "422": wire("`InvalidInput` of a single call."),
        "429": {
          "description": "`ResourceExhausted` of a single call. Problem: the rate limit.",
          "content": {
            "application/json": { "schema": result },
            PROBLEM_JSON: { "schema": problem },
          },
        },
        "500": wire("`InternalError` of a single call."),
        "504": wire("`DeadlineExceeded` of a single call."),
      },
    },
  })
}

fn rpc_header_param() -> Value {
  json!({
    "name": RPC_HEADER,
    "in": "header",
    "required": true,
    "description": "Every `POST` sends `x-rpc: 1` (C5): without it the call is a 403 Problem.",
    "schema": { "type": "string", "const": "1" },
  })
}

fn deadline_param() -> Value {
  json!({
    "name": DEADLINE_HEADER,
    "in": "header",
    "required": false,
    "description": "The requested deadline in milliseconds, clamped to the server's maximum (30 s by default).",
    "schema": { "type": "integer", "minimum": 0 },
  })
}

fn correlation_param() -> Value {
  json!({
    "name": CORRELATION_HEADER,
    "in": "header",
    "required": false,
    "description": "Echoed in the response; the `id` of an `InternalError`.",
    "schema": { "type": "string" },
  })
}

fn header_doc(description: &str) -> Value {
  json!({ "description": description, "schema": { "type": "string" } })
}

fn problem_response(description: &str) -> Value {
  json!({
    "description": description,
    "content": { PROBLEM_JSON: { "schema": schema_ref("rpc-Problem") } },
  })
}

fn problem_schema() -> Value {
  json!({
    "type": "object",
    "description": "RFC 9457 problem details of a transport-level rejection (never a `WireResult`).",
    "properties": {
      "type": { "type": "string", "default": "about:blank" },
      "title": { "type": "string" },
      "status": { "type": "integer", "minimum": 100, "maximum": 599 },
      "detail": { "type": "string" },
      "instance": { "type": "string" },
      "code": { "type": "string", "description": "A stable code where one exists (`IdempotencyKeyTooShort`)." },
      "tag": { "type": "string", "description": "The procedure tag the rejection is about." },
      "kind": { "type": "string", "description": "The procedure's kind, when it is the wrong one." },
      "version": { "type": "integer", "description": "The version that was asked for." },
    },
  })
}

/// One tag per service name; the newest version's description wins.
pub(crate) fn service_tags(contract: &Contract) -> Vec<Value> {
  let mut tags: BTreeMap<&str, Option<&str>> = BTreeMap::new();
  for service in &contract.services {
    let entry = tags.entry(&service.name).or_default();
    if service.description.is_some() {
      *entry = service.description.as_deref();
    }
  }
  tags
    .into_iter()
    .map(|(name, description)| match description {
      Some(d) => json!({ "name": name, "description": d }),
      None => json!({ "name": name }),
    })
    .collect()
}

// ---------------------------------------------------------------------------------------------
// Shared with `asyncapi`: tags, frames, exits, and the component registry
// ---------------------------------------------------------------------------------------------

/// The reserved failures a call over HTTP can end with.
pub(crate) const RESERVED_UNARY: [&str; 5] = [
  "InternalError",
  "InvalidInput",
  "DeadlineExceeded",
  "Cancelled",
  "ResourceExhausted",
];

/// A procedure version: its service and itself.
pub(crate) type Version<'a> = (&'a ServiceContract, &'a ProcedureContract);

/// Every procedure version by tag: tags sorted, versions ascending.
pub(crate) fn by_tag(contract: &Contract) -> BTreeMap<String, Vec<Version<'_>>> {
  let mut tags: BTreeMap<String, Vec<Version<'_>>> = BTreeMap::new();
  for service in &contract.services {
    for procedure in &service.procedures {
      tags
        .entry(format!("{}.{}", service.name, procedure.name))
        .or_default()
        .push((service, procedure));
    }
  }
  for versions in tags.values_mut() {
    versions.sort_by_key(|(service, _)| service.version);
  }
  tags
}

/// `<Service>-v<N>-<procedure>`: the prefix of a procedure version's generated components.
pub(crate) fn procedure_base(service: &ServiceContract, procedure: &ProcedureContract) -> String {
  format!(
    "{}-v{}-{}",
    ident(&service.name),
    service.version,
    ident(&procedure.name)
  )
}

/// `path` with a leading and without a trailing `/` (`""` for the root).
pub(crate) fn prefix(path: &str) -> String {
  let trimmed = path.trim_end_matches('/');
  if trimmed.is_empty() || trimmed.starts_with('/') {
    trimmed.to_owned()
  } else {
    format!("/{trimmed}")
  }
}

pub(crate) fn access_value(access: &ProcedureAccess) -> Value {
  serde_json::to_value(access).unwrap_or(Value::Null)
}

pub(crate) fn schema_ref(name: &str) -> Value {
  json!({ "$ref": format!("#/components/schemas/{name}") })
}

pub(crate) fn call_id_schema() -> Value {
  json!({
    "type": "integer",
    "minimum": 0,
    "maximum": MAX_CALL_ID,
    "description": "The call id chosen by the client (echoed); 0 is the connection itself.",
  })
}

/// `WireExit`: `Success` with `value`, or `Failure` with `error`.
pub(crate) fn exit_schema(value: Value, error: Value) -> Value {
  json!({
    "oneOf": [
      {
        "type": "object",
        "required": ["_tag", "value"],
        "properties": { "_tag": { "const": "Success" }, "value": value },
      },
      {
        "type": "object",
        "required": ["_tag", "error"],
        "properties": { "_tag": { "const": "Failure" }, "error": error },
      },
    ],
  })
}

/// A `Frame` of `kind` carrying `payload` (`Value::Null`: no payload member), plus `extra`
/// properties (`(name, schema, required)`).
pub(crate) fn frame_schema(kind: &str, payload: Value, extra: &[(&str, Value, bool)]) -> Value {
  let mut properties = Map::new();
  properties.insert("id".to_owned(), call_id_schema());
  properties.insert("kind".to_owned(), json!({ "const": kind }));
  properties.insert(
    "flags".to_owned(),
    json!({
      "type": "integer",
      "minimum": 0,
      "maximum": 3,
      "default": 0,
      "description": "Bit 0 `END` (half-close), bit 1 `ACK` (the item needs a credit back).",
    }),
  );
  let mut required = vec![json!("id"), json!("kind")];
  if !payload.is_null() {
    properties.insert("payload".to_owned(), payload);
    required.push(json!("payload"));
  }
  for (name, schema, needed) in extra {
    properties.insert((*name).to_owned(), schema.clone());
    if *needed {
      required.push(json!(name));
    }
  }
  json!({ "type": "object", "required": required, "properties": properties })
}

/// Whether `schema` admits `null` (a missing `?input=` is `null`).
fn accepts_null(schema: &Value) -> bool {
  let is_null = match schema.get("type") {
    Some(Value::String(t)) => t == "null",
    Some(Value::Array(types)) => types.iter().any(|t| t == "null"),
    _ => false,
  };
  if is_null || *schema == Value::Bool(true) || schema.as_object().is_some_and(Map::is_empty) {
    return true;
  }
  ["anyOf", "oneOf"].iter().any(|key| {
    schema
      .get(*key)
      .and_then(Value::as_array)
      .is_some_and(|arms| arms.iter().any(accepts_null))
  })
}

/// ASCII letters, digits and `_`; everything else becomes `_` (`_` when empty).
pub(crate) fn ident(text: &str) -> String {
  let out: String = text
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '_' {
        c
      } else {
        '_'
      }
    })
    .collect();
  if out.is_empty() { "_".to_owned() } else { out }
}

/// `value` with every object's keys sorted (a no-op unless serde_json keeps insertion order).
pub(crate) fn canonical(value: Value) -> Value {
  match value {
    Value::Object(map) => {
      let sorted: BTreeMap<String, Value> =
        map.into_iter().map(|(k, v)| (k, canonical(v))).collect();
      Value::Object(sorted.into_iter().collect())
    }
    Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
    other => other,
  }
}

/// Two-space indented JSON with a trailing newline.
pub(crate) fn pretty(value: &Value) -> String {
  // A tree of `Value`s always serializes.
  let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
  text.push('\n');
  text
}

fn text_of(value: &Value) -> String {
  serde_json::to_string(&canonical(value.clone())).unwrap_or_default()
}

fn hash8(parts: &[&str]) -> String {
  let mut hasher = Sha256::new();
  for part in parts {
    hasher.update(part.as_bytes());
    hasher.update([0]);
  }
  hasher.finalize()[..4]
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}

/// How a document writes discriminators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
  /// `{"propertyName": "_tag", "mapping": {…}}`.
  OpenApi,
  /// `"_tag"` (the AsyncAPI Schema Object's string form).
  AsyncApi,
}

/// The `components/schemas` of a document: hoisted `$defs`, error variants and generated
/// components (see the [module docs](self) for the naming rules).
pub(crate) struct Components {
  dialect: Dialect,
  /// `(definition key, body text)` → component name.
  defs: BTreeMap<(String, String), String>,
  /// `(tag, imported variant text)` → component name.
  variants: BTreeMap<(String, String), String>,
  schemas: BTreeMap<String, Value>,
}

impl Components {
  /// The registry for every schema of `contract` and the `extra` root schemas.
  pub(crate) fn for_contract(dialect: Dialect, contract: &Contract, extra: &[&Value]) -> Self {
    let mut roots: Vec<&Value> = Vec::new();
    let mut unions: Vec<&Value> = Vec::new();
    for service in &contract.services {
      for p in &service.procedures {
        roots.push(&p.input);
        roots.push(&p.output);
        if let Some(error) = &p.error {
          roots.push(error);
          unions.push(error);
        }
      }
    }
    roots.extend_from_slice(extra);
    let mut c = Self {
      dialect,
      defs: BTreeMap::new(),
      variants: BTreeMap::new(),
      schemas: BTreeMap::new(),
    };
    c.name_defs(&roots);
    c.name_variants(&unions);
    c
  }

  /// Name every distinct `(key, body)` of the roots' `$defs`, then register the bodies.
  fn name_defs(&mut self, roots: &[&Value]) {
    // (key, text) → (body, the first root holding it)
    let mut found: BTreeMap<(String, String), (Value, usize)> = BTreeMap::new();
    for (i, root) in roots.iter().enumerate() {
      for (key, body) in defs_of(root) {
        found
          .entry((key.clone(), text_of(body)))
          .or_insert_with(|| (body.clone(), i));
      }
    }
    let keys: Vec<&str> = found
      .keys()
      .map(|(k, _)| k.as_str())
      .collect::<BTreeSet<_>>()
      .into_iter()
      .collect();
    let bases = def_names(&keys);
    let mut taken: BTreeMap<String, usize> = BTreeMap::new();
    for (key, text) in found.keys() {
      let base = bases[key.as_str()].clone();
      let n = taken.entry(base.clone()).or_insert(0);
      *n += 1;
      let name = if *n == 1 { base } else { format!("{base}_{n}") };
      self.defs.insert((key.clone(), text.clone()), name);
    }
    for (pair, (body, root)) in &found {
      let tokens = self.tokens(roots[*root]);
      let mut body = body.clone();
      rewrite_refs(&mut body, &tokens);
      let name = self.defs[pair].clone();
      self.schemas.insert(name, canonical(body));
    }
  }

  /// Name the error variants of `unions` and the reserved failures.
  fn name_variants(&mut self, unions: &[&Value]) {
    let mut bodies: BTreeMap<(String, String), Value> = BTreeMap::new();
    for union in unions {
      let imported = self.import(union);
      for variant in variant_list(&imported) {
        if let Some(tag) = variant_tag(variant) {
          bodies
            .entry((tag.to_owned(), text_of(variant)))
            .or_insert_with(|| variant.clone());
        }
      }
    }
    for tag in RpcFailure::RESERVED_TAGS {
      let body = reserved_schema(tag);
      bodies
        .entry((tag.to_owned(), text_of(&body)))
        .or_insert(body);
    }
    let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for pair in bodies.keys() {
      groups
        .entry(format!("error-{}", ident(&pair.0)))
        .or_default()
        .push(pair.clone());
    }
    for (base, members) in groups {
      let single = members.len() == 1;
      for pair in members {
        let name = if single {
          base.clone()
        } else {
          format!("{base}-{}", hash8(&[&pair.0, &pair.1]))
        };
        let body = bodies[&pair].clone();
        self.schemas.insert(name.clone(), canonical(body));
        self.variants.insert(pair, name);
      }
    }
  }

  /// `#/$defs/<token>` → component name for `root`'s definitions.
  fn tokens(&self, root: &Value) -> BTreeMap<String, String> {
    defs_of(root)
      .into_iter()
      .filter_map(|(key, body)| {
        let name = self.defs.get(&(key.clone(), text_of(body)))?;
        let token = key.replace('~', "~0").replace('/', "~1");
        Some((token, name.clone()))
      })
      .collect()
  }

  /// `root` without `$schema` and `$defs`, its references pointing at the components.
  pub(crate) fn import(&self, root: &Value) -> Value {
    let tokens = self.tokens(root);
    let mut schema = root.clone();
    if let Value::Object(map) = &mut schema {
      map.remove("$schema");
      map.remove("$defs");
    }
    rewrite_refs(&mut schema, &tokens);
    canonical(schema)
  }

  /// Register (or replace) a generated component.
  pub(crate) fn insert(&mut self, name: &str, schema: Value) {
    self.schemas.insert(name.to_owned(), canonical(schema));
  }

  /// `<base>-Error`: the procedure's typed variants and the `reserved` failures as a `oneOf`
  /// discriminated by `_tag`. Returns the component name.
  pub(crate) fn error_component(
    &mut self,
    base: &str,
    procedure: &ProcedureContract,
    reserved: &[&str],
  ) -> String {
    let name = format!("{base}-Error");
    let mut arms = Vec::new();
    let mut mapping = Map::new();
    let mut add = |arms: &mut Vec<Value>, tag: &str, component: &str| {
      arms.push(schema_ref(component));
      mapping.insert(
        tag.to_owned(),
        json!(format!("#/components/schemas/{component}")),
      );
    };
    if let Some(error) = &procedure.error {
      let imported = self.import(error);
      let list = variant_list(&imported);
      if list.is_empty() {
        arms.push(imported.clone());
      }
      for variant in list {
        let named = variant_tag(variant)
          .and_then(|tag| Some((tag, self.variants.get(&(tag.to_owned(), text_of(variant)))?)));
        match named {
          Some((tag, component)) => add(&mut arms, tag, component),
          None => arms.push(variant.clone()),
        }
      }
    }
    for tag in reserved {
      let body = reserved_schema(tag);
      if let Some(component) = self.variants.get(&((*tag).to_owned(), text_of(&body))) {
        add(&mut arms, tag, component);
      }
    }
    let discriminator = match self.dialect {
      Dialect::OpenApi => json!({ "propertyName": "_tag", "mapping": mapping }),
      Dialect::AsyncApi => json!("_tag"),
    };
    let description = if procedure.error.is_some() {
      "The typed error union and the reserved failures, discriminated by `_tag`."
    } else {
      "The reserved failures (the procedure declares no typed error), discriminated by `_tag`."
    };
    self.insert(
      &name,
      json!({ "description": description, "oneOf": arms, "discriminator": discriminator }),
    );
    name
  }

  /// The registered schemas.
  pub(crate) fn into_schemas(self) -> Map<String, Value> {
    self.schemas.into_iter().collect()
  }
}

/// The `$defs` of `root`, sorted by key.
fn defs_of(root: &Value) -> Vec<(&String, &Value)> {
  let mut list: Vec<_> = root
    .get("$defs")
    .and_then(Value::as_object)
    .map(|defs| defs.iter().collect())
    .unwrap_or_default();
  list.sort_by(|a, b| a.0.cmp(b.0));
  list
}

/// The `oneOf` arms of an error union (empty when it is no `oneOf`).
fn variant_list(union: &Value) -> Vec<&Value> {
  union
    .get("oneOf")
    .and_then(Value::as_array)
    .map(|arms| arms.iter().collect())
    .unwrap_or_default()
}

fn variant_tag(variant: &Value) -> Option<&str> {
  variant
    .get("properties")?
    .get("_tag")?
    .get("const")?
    .as_str()
}

/// The schema of a reserved failure (design note §4).
pub(crate) fn reserved_schema(tag: &str) -> Value {
  let (description, fields) = match tag {
    "InternalError" => (
      "A defect or an unexpected interrupt: only the correlation id, never a message (C10).",
      json!({ "id": { "type": "string", "description": "The correlation id (`x-correlation-id`)." } }),
    ),
    "InvalidInput" => (
      "The input failed Schema decode; every issue at once.",
      json!({
        "issues": {
          "type": "array",
          "items": {
            "type": "object",
            "required": ["path", "message"],
            "properties": {
              "path": { "type": "string", "description": "JSON Pointer into the input." },
              "message": { "type": "string" },
            },
          },
        },
      }),
    ),
    "ResourceExhausted" => (
      "A per-call limit (items, bytes, credits) was exceeded.",
      json!({ "limit": { "type": "string", "description": "Which limit." } }),
    ),
    "DeadlineExceeded" => ("The server-capped deadline expired.", json!({})),
    "Cancelled" => (
      "The client cancelled or disconnected (status 499, only logged).",
      json!({}),
    ),
    _ => ("A resume came after the replay buffer overran.", json!({})),
  };
  let mut properties = Map::new();
  properties.insert("_tag".to_owned(), json!({ "const": tag }));
  let mut required = vec![json!("_tag")];
  if let Value::Object(fields) = fields {
    for (name, schema) in fields {
      required.push(json!(name));
      properties.insert(name, schema);
    }
  }
  json!({
    "type": "object",
    "description": description,
    "required": required,
    "properties": properties,
  })
}

/// Component base names for definition keys (see the [module docs](self)).
fn def_names(keys: &[&str]) -> BTreeMap<String, String> {
  let segments: Vec<Vec<String>> = keys.iter().map(|k| key_segments(k)).collect();
  let suffix = |segs: &[String], n: usize| segs[segs.len().saturating_sub(n)..].join(".");
  let mut out = BTreeMap::new();
  for (i, segs) in segments.iter().enumerate() {
    let name = (1..=segs.len())
      .map(|n| (n, suffix(segs, n)))
      .find(|(n, candidate)| {
        !segments
          .iter()
          .enumerate()
          .any(|(j, other)| j != i && other.len() >= *n && suffix(other, *n) == *candidate)
      })
      .map_or_else(|| suffix(segs, segs.len()), |(_, name)| name);
    out.insert(keys[i].to_owned(), name);
  }
  out
}

fn key_segments(key: &str) -> Vec<String> {
  let segs: Vec<String> = key
    .split("::")
    .map(|seg| ident(seg).trim_end_matches('_').to_owned())
    .filter(|seg| !seg.is_empty())
    .collect();
  if segs.is_empty() {
    vec!["Schema".to_owned()]
  } else {
    segs
  }
}

/// Point `#/$defs/<token>` references at the components; literal values (`const`, `enum`,
/// `examples`, `default`) are left alone.
fn rewrite_refs(value: &mut Value, tokens: &BTreeMap<String, String>) {
  match value {
    Value::Object(map) => {
      let target = map
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/$defs/"))
        .and_then(|token| tokens.get(token));
      if let Some(name) = target {
        let pointer = format!("#/components/schemas/{name}");
        map.insert("$ref".to_owned(), Value::String(pointer));
      }
      for (key, child) in map.iter_mut() {
        if !matches!(key.as_str(), "const" | "enum" | "examples" | "default") {
          rewrite_refs(child, tokens);
        }
      }
    }
    Value::Array(items) => {
      for item in items {
        rewrite_refs(item, tokens);
      }
    }
    _ => {}
  }
}

// ---------------------------------------------------------------------------------------------
// Serving the documents (ADR 0013 R7)
// ---------------------------------------------------------------------------------------------

/// The variable naming the profile the process runs under (as in `id_effect_config`).
pub const PROFILE_ENV: &str = "ID_EFFECT_PROFILE";

/// The profiles in which unguarded documents are mounted.
const OPEN_PROFILES: [&str; 2] = ["dev", "test"];

/// `{path}/openapi.json`.
pub const OPENAPI_JSON: &str = "openapi.json";
/// `{path}/asyncapi.json`.
pub const ASYNCAPI_JSON: &str = "asyncapi.json";
/// `{path}/_contract`: the contract itself (`rpc.contract.json`).
pub const CONTRACT_JSON: &str = "_contract";

/// Serves the OpenAPI, AsyncAPI and contract documents of a [`Contract`] next to the `/rpc`
/// routes; closed outside `dev` and `test` unless guarded (see the [module docs](self)).
///
/// ```rust
/// use axum::body::Body;
/// use axum::http::{Request, StatusCode};
/// use id_effect_rpc::contract::Contract;
/// use id_effect_rpc::openapi::ContractDocs;
/// use tower::ServiceExt;
///
/// let contract = Contract::from_json(include_str!("../rpc.contract.json")).unwrap();
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let get = |uri: &str| Request::get(uri).body(Body::empty()).unwrap();
/// let prod: axum::Router = ContractDocs::new(contract.clone()).profile("prod").router();
/// let res = prod.oneshot(get("/rpc/openapi.json")).await.unwrap();
/// assert_eq!(res.status(), StatusCode::NOT_FOUND);
/// let dev: axum::Router = ContractDocs::new(contract).profile("dev").router();
/// let res = dev.oneshot(get("/rpc/asyncapi.json")).await.unwrap();
/// assert_eq!(res.status(), StatusCode::OK);
/// # });
/// ```
#[derive(Clone)]
#[must_use]
pub struct ContractDocs {
  contract: Contract,
  openapi: OpenApiOptions,
  asyncapi: AsyncApiOptions,
  profile: Option<String>,
  guards: Vec<Arc<dyn RouteGuard>>,
  public: bool,
}

impl fmt::Debug for ContractDocs {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let policies: Vec<&str> = self.guards.iter().map(|g| g.policy()).collect();
    f.debug_struct("ContractDocs")
      .field("services", &self.contract.services.len())
      .field("openapi", &self.openapi)
      .field("asyncapi", &self.asyncapi)
      .field("profile", &self.profile)
      .field("guards", &policies)
      .field("public", &self.public)
      .finish()
  }
}

impl ContractDocs {
  /// The documents of `contract` with default options, unguarded (mounted in `dev` and `test`).
  pub fn new(contract: Contract) -> Self {
    Self {
      contract,
      openapi: OpenApiOptions::default(),
      asyncapi: AsyncApiOptions::default(),
      profile: None,
      guards: Vec::new(),
      public: false,
    }
  }

  /// The OpenAPI options; their `path` is where the three routes are mounted.
  pub fn openapi(mut self, options: OpenApiOptions) -> Self {
    self.openapi = options;
    self
  }

  /// The AsyncAPI options.
  pub fn asyncapi(mut self, options: AsyncApiOptions) -> Self {
    self.asyncapi = options;
    self
  }

  /// The profile the app runs under (default: [`PROFILE_ENV`] when the router is built, else
  /// `dev`).
  pub fn profile(mut self, profile: impl Into<String>) -> Self {
    self.profile = Some(profile.into());
    self
  }

  /// Put every route behind `guard` (repeatable, outermost last), in every profile; guarded
  /// documents are mounted in `prod` too.
  pub fn guard(mut self, guard: impl RouteGuard) -> Self {
    self.guards.push(Arc::new(guard));
    self
  }

  /// Serve the documents to anyone in every profile, `prod` included. Recorded in the posture
  /// registry as `id_effect_rpc::openapi::ContractDocs::public_dangerous`.
  pub fn public_dangerous(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      "id_effect_rpc::openapi::ContractDocs::public_dangerous",
      "R7: the RPC contract, OpenAPI and AsyncAPI documents are served to anonymous callers in prod",
    );
    self.public = true;
    self
  }

  /// Whether [`router`](Self::router) mounts the routes: guarded, explicitly public, or in the
  /// `dev` or `test` profile.
  pub fn is_mounted(&self) -> bool {
    if self.public || !self.guards.is_empty() {
      return true;
    }
    let profile = self
      .profile
      .clone()
      .or_else(|| std::env::var(PROFILE_ENV).ok())
      .filter(|p| !p.trim().is_empty());
    profile.is_none_or(|p| OPEN_PROFILES.contains(&p.trim()))
  }

  /// `GET {path}/openapi.json`, `{path}/asyncapi.json` and `{path}/_contract` (JSON, a strong
  /// `ETag`, `Cache-Control: no-cache`, `If-None-Match` → 304) behind the guards (outermost,
  /// i.e. last added, first); an empty router when not [mounted](Self::is_mounted). The
  /// documents are built now.
  pub fn router<S>(&self) -> axum::Router<S>
  where
    S: Clone + Send + Sync + 'static,
  {
    if !self.is_mounted() {
      tracing::info!(
        target: "id_effect_rpc::openapi",
        path = %self.openapi.path,
        "RPC contract documents not mounted: unguarded outside the dev and test profiles",
      );
      return axum::Router::new();
    }
    let prefix = prefix(&self.openapi.path);
    let docs = [
      (OPENAPI_JSON, to_json(&self.contract, &self.openapi)),
      (
        ASYNCAPI_JSON,
        asyncapi::to_json(&self.contract, &self.asyncapi),
      ),
      (CONTRACT_JSON, self.contract.to_json_pretty()),
    ];
    let mut router = axum::Router::new();
    for (name, text) in docs {
      let body = Bytes::from(text);
      let digest = Sha256::digest(&body);
      let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
      let etag = HeaderValue::from_str(&format!("\"{hex}\"")).expect("hex is a header value");
      router = router.route(
        &format!("{prefix}/{name}"),
        axum::routing::get(move |headers: HeaderMap| {
          std::future::ready(serve(&headers, &body, &etag))
        }),
      );
    }
    // `route_layer` wraps: the guard added last runs first.
    self.guards.iter().fold(router, |router, guard| {
      let guard = Arc::clone(guard);
      router.route_layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
          let guard = Arc::clone(&guard);
          async move {
            let (parts, body) = request.into_parts();
            match guard.check(&parts).await {
              Ok(()) => {
                next
                  .run(axum::extract::Request::from_parts(parts, body))
                  .await
              }
              Err(rejection) => rejection,
            }
          }
        },
      ))
    })
  }
}

fn serve(headers: &HeaderMap, body: &Bytes, etag: &HeaderValue) -> Response {
  let current = headers
    .get_all(header::IF_NONE_MATCH)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|v| v.split(','))
    .any(|v| {
      let v = v.trim();
      v == "*" || v.trim_start_matches("W/").as_bytes() == etag.as_bytes()
    });
  let mut response = if current {
    StatusCode::NOT_MODIFIED.into_response()
  } else {
    let json = HeaderValue::from_static("application/json");
    ([(header::CONTENT_TYPE, json)], body.clone()).into_response()
  };
  let h = response.headers_mut();
  h.insert(header::ETAG, etag.clone());
  h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
  response
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn def_names_take_the_shortest_unique_suffix() {
    let names = def_names(&[
      "a::Item",
      "b::Item",
      "c::Todo",
      "x::y::Page_b::Item_",
      "Solo",
    ]);
    assert_eq!(names["a::Item"], "a.Item");
    assert_eq!(names["b::Item"], "b.Item");
    assert_eq!(names["c::Todo"], "Todo");
    assert_eq!(names["x::y::Page_b::Item_"], "Page_b.Item");
    assert_eq!(names["Solo"], "Solo");
    let dotted = def_names(&["a.b::c", "a::b.c"]);
    assert_eq!(dotted["a.b::c"], "c");
    assert_eq!(dotted["a::b.c"], "b_c");
    let same = def_names(&["x::T", "x::T"]);
    assert_eq!(same["x::T"], "x.T");
    assert_eq!(key_segments("::_::"), ["Schema"]);
    assert_eq!(ident(""), "_");
  }

  #[test]
  fn null_inputs_are_optional_query_parameters() {
    assert!(accepts_null(&json!({"type": "null"})));
    assert!(accepts_null(&json!({"type": ["string", "null"]})));
    assert!(accepts_null(
      &json!({"anyOf": [{"type": "string"}, {"type": "null"}]})
    ));
    assert!(accepts_null(&json!({})));
    assert!(accepts_null(&json!(true)));
    assert!(!accepts_null(&json!({"type": "integer"})));
    assert!(!accepts_null(&json!({"type": 3})));
  }

  #[test]
  fn prefixes_are_normalized() {
    assert_eq!(prefix("/rpc/"), "/rpc");
    assert_eq!(prefix("rpc"), "/rpc");
    assert_eq!(prefix("/"), "");
  }

  #[test]
  fn reserved_failures_have_their_fields() {
    let internal = reserved_schema("InternalError");
    assert_eq!(internal["required"], json!(["_tag", "id"]));
    assert_eq!(
      reserved_schema("ResumeExpired")["required"],
      json!(["_tag"])
    );
    assert_eq!(
      reserved_schema("InvalidInput")["properties"]["issues"]["type"],
      "array"
    );
    assert_eq!(hash8(&["a", "b"]).len(), 8);
  }
}
