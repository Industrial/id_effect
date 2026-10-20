# Contracts as OpenAPI and AsyncAPI

The service contract (`rpc.contract.json`, chapter 33.4) is the source of truth for clients.
Two standard documents are generated from it for everything that does not read the contract
directly: **OpenAPI 3.1** for the calls a plain HTTP client makes, and **AsyncAPI 3.0** for the
streams that run over WebSocket and server-sent events.

```rust
use id_effect_rpc::asyncapi::{self, AsyncApiOptions, ChannelTopic};
use id_effect_rpc::contract::{
  Contract, ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract,
};
use id_effect_rpc::openapi::{self, OpenApiOptions};
use serde_json::json;

// `Todos::document()` with `#[rpc::service]`, or `Contract::from_json` of the snapshot.
let get = ProcedureContract::new(
  "get",
  ProcedureKind::Query,
  ProcedureAccess::Public {},
  json!({"type": "integer"}),
  json!({"type": "string"}),
);
let contract = Contract::new().with_service(ServiceContract::new("Todos", 1).with_procedure(get));

let http = openapi::to_json(&contract, &OpenApiOptions::new("Todos API", "1.0.0"));
let streams = asyncapi::to_json(
  &contract,
  &AsyncApiOptions::new("Todos streams", "1.0.0")
    .with_host("api.example.com")
    .with_topic(ChannelTopic::new("lobby", "Chat.join", json!({"type": "string"}))),
);
assert!(http.contains("\"openapi\": \"3.1.0\""));
assert!(streams.contains("\"asyncapi\": \"3.0.0\""));
```

Both functions are pure: the same contract gives the same bytes (keys sorted, services by name
and version, procedures by name), so the documents can be checked in and diffed in review.

## What OpenAPI describes

| Kind | Operations at `/rpc/{tag}` |
|---|---|
| query | `GET ?input=<JSON>` with `ETag` / `If-None-Match` → 304, and `POST` with the input as body |
| mutation | `POST` with the optional `Idempotency-Key` (16 characters at least on a public mutation) |
| server stream | `POST`, answered with `application/x-ndjson`: one `item` frame per output, one `exit` |
| client stream | `POST` with `application/x-ndjson` items (the contract's `max_items` / `max_bytes`) |

`POST /rpc` is the envelope route: one `WireCall`, or a batch of at most 50 calls. Its request
body lists every `(tag, version)` of the contract, so older service versions are described
there; the `/rpc/{tag}` operation describes the newest version, the one a call without
`x-rpc-version` gets, and lists the served versions in that header's `enum`.

Every `POST` documents the required `x-rpc: 1` header. Responses are split the way the server
answers: a `WireResult` once a procedure ran (200; the typed error's status, 400 by default;
422 `InvalidInput`, 429 `ResourceExhausted`, 500 `InternalError`, 504 `DeadlineExceeded`), and
an RFC 9457 Problem for transport rejections (400, 401/403 from a guard, 403 without `x-rpc`,
404, 409 for an idempotency conflict, 413, 415, 429). Access decisions travel as `x-rpc-access`.

The typed error union of a procedure becomes the component `<Service>-v<N>-<proc>-Error`, a
`oneOf` of its variants plus the reserved failures with a `_tag` discriminator:

```json
{
  "discriminator": {
    "mapping": { "Locked": "#/components/schemas/error-Locked", "NotFound": "#/components/schemas/error-NotFound" },
    "propertyName": "_tag"
  },
  "oneOf": [{ "$ref": "#/components/schemas/error-NotFound" }, { "$ref": "#/components/schemas/error-Locked" }]
}
```

Types under `$defs` are hoisted into `components/schemas`, named after the last segment of their
Rust path (`app::todo::Item` → `Item`). When two types share that segment, both get the shortest
path suffix that tells them apart (`todo.Item`, `tag.Item`), whatever order they were seen in.
Names the generator adds itself always contain a `-` (`rpc-Problem`, `error-NotFound`,
`Todos-v1-get-Result`), so they never collide with a type. Rust type paths can change with the
compiler, and the document with them.

## What AsyncAPI describes

The document is written from the server's side: a channel's `receive` operation lists what a
client sends, its `send` operation what the client gets back.

- **WebSocket** (`ws` server, subprotocols `idrpc.json.v1` and `idrpc.postcard.v1`): one channel
  per stream procedure version (`Todos-v1-ticks.ws`, address `Todos.ticks`) with its frames:
  `call`, `clientItem`, `end`, `clientCredit`, `cancel` from the client; `serverItem`,
  `serverCredit`, `exit` from the server, as the kind needs them. The `connection` channel has
  the frames of call id 0: `hello`, `resume`, `ping`, `pong`.
- **Server-sent events** (`sse` server): server streams and subscriptions at `GET /rpc/{tag}`,
  events `item` and `exit`. A subscription's event `id:` is its cursor; reconnecting with
  `Last-Event-ID` resumes after it.
- **Topics**: each `ChannelTopic` adds `topic.<name>.ws` and `topic.<name>.sse` channels: the
  topic's join procedure (`Channels::serve_joins`, input `{"topic": <name>}`) and its
  `joined`, `message` and `presence` events (chapter 33.9).

The cursor (`c1.` and 12 base64url characters) is documented where it travels, the `cursor`
member of `item` frames and the SSE event id, not inside the procedure's output schema.

## Serving them (closed in prod)

`ContractDocs` serves `/rpc/openapi.json`, `/rpc/asyncapi.json` and `/rpc/_contract`. They list
every procedure, so ADR 0013 R7 applies: outside the `dev` and `test` profiles nothing is
mounted unless the routes are guarded. Unmounted, the paths fall through to the RPC routes and
answer 404.

```rust
use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use id_effect_axum::route::GuardFuture;
use id_effect_rpc::contract::Contract;
use id_effect_rpc::openapi::ContractDocs;
use id_effect_rpc::registry::RouteGuard;
use tower::ServiceExt;

struct Operators;
impl RouteGuard for Operators {
  fn policy(&self) -> &str {
    "operators"
  }
  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let ok = parts.headers.contains_key("x-operator");
    Box::pin(async move { if ok { Ok(()) } else { Err(StatusCode::FORBIDDEN.into_response()) } })
  }
}

let contract = Contract::new(); // the app's contract
let docs: axum::Router = ContractDocs::new(contract).profile("prod").guard(Operators).router();
# tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
let res = docs.oneshot(Request::get("/rpc/openapi.json").body(Body::empty()).unwrap()).await.unwrap();
assert_eq!(res.status(), StatusCode::FORBIDDEN);
# });
```

`ContractDocs::public_dangerous()` serves them to anyone in every profile and records the
opt-out in the posture registry. Each document is built once when the router is, served with a
strong `ETag` and `Cache-Control: no-cache`.

## Checking them

`crates/id_effect_rpc/tests/openapi_emit.rs` validates the OpenAPI document against the official
OpenAPI 3.1 schema and every component against JSON Schema 2020-12;
`tests/asyncapi_emit.rs` validates the AsyncAPI document against the official AsyncAPI 3.0.0
schema, vendored unchanged as `tests/fixtures/asyncapi-3.0.schema.json`. Neither fetches
anything: `jsonschema` runs without remote resolution. Both tests also check real `WireResult`s
and frames against the emitted schemas.
