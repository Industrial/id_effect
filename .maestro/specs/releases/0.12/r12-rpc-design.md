---
title: 'id_effect_rpc 0.12 wire and contract design note (r12-e12-1-promote)'
slug: r12-rpc-design
status: proposed — review (owner or coordinator) before r12-e12-1-kinds starts
binding_for: r12-e12-1-kinds, r12-e12-1-batching, r12-e12-1-postcard, r12-e12-1-macro, r12-e12-2-rpc-ws-sse, r12-e12-2-subscriptions, r12-e12-2-bidi-resume, r12-e12-2-channels, r12-e12-2-mesh-rpc, r12-e12-3-webtransport, r12-e12-4-*
---

# id_effect_rpc 0.12 — contract, kinds, wire, frames, cursors

ADR 0014 decides *what* the call surface is; this note fixes *how it looks on the wire* so that the
wave 2–5 leaves do not reshape it. Where this note and ADR 0014 disagree, the ADR wins and this note
is corrected. Every format below is versioned (`1` in a name, a `v1` suffix or a leading version
byte), so 0.13+ can add a format beside it instead of changing it.

What already exists after r12-e12-1-promote (crate `crates/id_effect_rpc`, tier `integration`):

| Module | State after promote | Next owner |
|---|---|---|
| `contract` | D11 serde model below, `Contract::to_json_pretty`, `Contract::from_json`, `assert_snapshot` (golden, `UPDATE_GOLDEN=1`) | macro fills it from `#[rpc::service]` |
| `wire` | the 0.12 envelope types below (`CallId`, `WireCall`, `WireExit`, `WireResult`, `RpcFailure`, `Frame`, `FrameKind`, `FrameFlags`, `Cursor`, `SessionId`, `Batch`) with serde and round-trip tests | used by every transport leaf; not edited except to add a variant behind a new name |
| `codec/{mod,json}` | `Codec` trait, `CodecId`, `negotiate_content_type`, `negotiate_subprotocol`, `Json` | postcard adds `codec/postcard.rs` and the `CodecId::Postcard` arm |
| `registry` | `Procedure<A>` access typestate, `RpcGroup::register`, per-procedure guards, `RpcGroup::service_contract` | kinds (handler storage for the other five kinds) |
| `server/{mod,dispatch,http}` | `RpcServer` as a declared typed route; dispatch with guards; unknown tag 404 Problem | kinds (`dispatch`, `http`), batching (`http` batch branch), rpc-ws-sse (`mod` mounts) |
| `client/{mod,http}` | `RpcClient` over `HttpClientService` (feature `client`) | rpc-ws-sse (`mod` transport choice), batching (`http`) |
| `protocol`, `serialization`, `envelope`, `error`, `stream` | the labs tagged-JSON format (`{"kind":"success"…}`), unchanged | kinds moves the HTTP path to `wire` and deletes what it replaces |

## 1. Contract model (D11)

`contract::Contract` is the language-neutral description the macro generates
(`Service::contract()`), `rpc.contract.json` stores, and `id-effect rpc gen ts` / `rpc check` read.
Serialization is stable: fields in declaration order, services sorted by `(name, version)`,
procedures sorted by `name`, JSON pretty-printed with two-space indent and a trailing newline.

```json
{
  "format": "id-effect.rpc.contract/1",
  "services": [
    {
      "name": "Greeter",
      "version": 1,
      "description": "Says hello.",
      "procedures": [
        {
          "name": "greet",
          "kind": "query",
          "access": { "public": {} },
          "input":  { "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] },
          "output": { "type": "string" },
          "error":  { "oneOf": [ { "type": "object", "properties": { "_tag": { "const": "Unknown" } }, "required": ["_tag"] } ] },
          "description": "Greets someone."
        }
      ]
    }
  ]
}
```

- `format` is the contract format; a reader refuses another value.
- `version` (u32, ≥ 1) is the service contract version. Versions are served side by side; a client
  names one (§3.4). `rpc check` compares a procedure only against the same `(service, version)`.
- `kind` is one of the six kinds of §2 (`snake_case`).
- `access` is `{"public":{}}` or `{"guarded":{"policies":["staff", …]}}` — the per-procedure H8
  decision (C1). There is no undecided value: an undecided procedure cannot be registered.
- `input`, `output`, `error` are JSON Schema 2020-12 documents (the dialect OpenAPI 3.1 uses).
  For streams `input`/`output` describe one *item*. `error` is the typed error union (§4); `null`
  when the procedure has no typed error. Schemas come from `id_effect::schema` through
  `SchemaRef` (the same source typed routes use for OpenAPI).
- Optional per procedure: `description`, `deprecated` (bool), `idempotent` (mutations replayable by
  `Idempotency-Key`, §3.2), `max_items` / `max_bytes` (client stream caps).

Compatibility rules for `rpc check` (r12-e12-4-rpc-check implements them): adding a procedure,
an optional input field or an output field is compatible; removing or renaming a procedure,
changing its `kind`, tightening `access`, adding a required input field, removing an output field
or adding an error variant is breaking within a published version.

## 2. The six kinds and their HTTP mapping

The procedure tag on every transport is `"<Service>.<procedure>"` (e.g. `"Greeter.greet"`); the
low-level `RpcGroup` takes any tag of `[A-Za-z0-9_.-]{1,128}`. Prefix: `/rpc` (configurable,
`RPC_DISPATCH_PATH`).

| Kind | HTTP request | HTTP response | Other transports |
|---|---|---|---|
| `query` | `GET /rpc/{tag}?input=<url-encoded JSON>` (ETag on the encoded output, `If-None-Match` → 304), or `POST /rpc` envelope, or `POST /rpc/{tag}` with the input as body | 200 `WireResult` | WS/WT frames, mesh, batch |
| `mutation` | `POST /rpc` envelope or `POST /rpc/{tag}`; `Idempotency-Key` (≤ 128 visible ASCII) replays the stored `WireResult` (D14); never GET | 200 `WireResult` | WS/WT frames, mesh, batch |
| `server_stream` | `POST /rpc/{tag}`, `Accept: application/x-ndjson` | 200 NDJSON, one `Frame` (§5) per line: `item`… then one `exit` | WS/WT, SSE, mesh |
| `client_stream` | `POST /rpc/{tag}`, `Content-Type: application/x-ndjson`, one input item per line, chunked; item cap and byte cap from the contract / config | 200 `WireResult` | WS/WT, mesh |
| `bidi` | not on plain HTTP (426 Problem `UpgradeRequired`) | — | WS/WT only (credit backpressure, resume) |
| `subscription` | `GET /rpc/{tag}?input=…` with `Accept: text/event-stream` (SSE; `Last-Event-ID` = cursor) | SSE: `id:` = cursor, `event: item` / `event: exit`, `data:` = JSON | WS/WT, mesh |

Rules common to every kind:

- Only queries are retry-safe; only queries may ever be served by 0-RTT (C7).
- Mutations need `Content-Type: application/json` (or the postcard type) **and** the header
  `x-rpc: 1`; a form content type is refused (C5, `415`/`403` Problem). Queries over GET need no
  body and no `x-rpc`.
- Each call gets a server-capped deadline: `x-rpc-deadline-ms` (client request) is clamped to the
  server's `max_deadline` (default 30 s). Expiry interrupts the handler fiber and answers
  `DeadlineExceeded`.
- A client disconnect interrupts the handler fiber; its finalizers run (kinds:
  `client_disconnect_interrupts_handler_and_runs_finalizers`).
- Transport-level rejections are RFC 9457 Problem Details (`application/problem+json`), never a
  `WireResult`: unknown tag **404**, malformed envelope **400**, guard refusal (the guard's own
  response, typically 401/403), unknown codec **415**, body/batch over cap **413**, rate limit
  **429**. A `WireResult` is only produced once a procedure ran (or its input failed Schema decode,
  which is the typed `InvalidInput` failure, §4).

### 2.1 The HTTP envelope (unary)

`POST /rpc`, body `WireCall`:

```json
{"id": 7, "tag": "Greeter.greet", "v": 1, "payload": {"name": "Ada"}}
```

`id` is optional on a single call (echoed when present), required in a batch and on frames. `v`
is the service version (absent: the newest version served). Response body `WireResult`:

```json
{"id": 7, "tag": "Greeter.greet", "exit": {"_tag": "Success", "value": "Hello, Ada!"}}
{"id": 7, "tag": "Greeter.greet", "exit": {"_tag": "Failure", "error": {"_tag": "NotFound", "what": "user"}}}
```

HTTP status of a single call: `Success` 200; `Failure` with a typed error: the status the error
type declares (`RpcErrorStatus`, default **400**); the reserved failures of §4 their listed status.
A batch is always 200 with per-call exits.

### 2.2 Batch body (r12-e12-1-batching)

`POST /rpc` with a JSON **array** of `WireCall` (each with a distinct `id`), at most 50 calls and
1 MiB (config, C2; over → 413 Problem before decoding the elements). Header `x-rpc-ordered: true`
runs the calls sequentially in array order; otherwise they run with bounded parallelism through
compute admission. One `RequestCache` per batch. Response: a JSON array of `WireResult` **in
request order**, ids echoed, each call failing alone. Only `query` and `mutation` may be batched; a
stream kind in a batch is that call's `InvalidInput` failure. A batch is charged N rate-limit tokens.

## 3. Versions, idempotency, headers

1. `x-rpc: 1` — custom header every first-party client sends (C5).
2. `Idempotency-Key` — mutations; the store keys on `(principal, tag, key)`, TTL 24 h, 100,000 keys
   (D14); a replay with a different payload digest is `409` Problem.
3. `x-rpc-deadline-ms` — requested deadline (clamped).
4. `x-rpc-version: N` — version for `GET /rpc/{tag}` and `POST /rpc/{tag}` (the envelope uses `v`).
5. `x-correlation-id` — echoed; it is the `id` of `InternalError`.

## 4. Error model (C10)

`WireExit` is a tagged union on `_tag`:

| `_tag` | Fields | Meaning |
|---|---|---|
| `Success` | `value` | the procedure's output |
| `Failure` | `error` | a typed error, itself a tagged union `{"_tag": "<Variant>", …fields}` |

The typed error union is part of the contract (`error` schema). These **reserved** variants can
appear in any procedure's `Failure` without being declared, and a user error variant may not use
these names (the macro refuses them):

| Reserved `_tag` | Fields | HTTP status (single call) | When |
|---|---|---|---|
| `InternalError` | `id` (correlation id) | 500 | a defect or an unexpected interrupt; **no message, no cause** (C10) |
| `InvalidInput` | `issues: [{path, message}]` | 422 | the payload failed Schema decode |
| `DeadlineExceeded` | — | 504 | the capped deadline expired |
| `Cancelled` | — | 499 (logged only; the client is gone) | the client cancelled (frame `cancel`) or disconnected |
| `ResourceExhausted` | `limit` | 429 | a per-call limit (items, bytes, credits) |
| `ResumeExpired` | — | 410 | bidi resume after the buffer overran (`ResumeError::Expired`) |

`RpcErrorStatus` (a trait the macro implements for each error variant, default 400) maps a typed
variant to a status. The labs `RpcEnvelope {code, message}` stays as the *Problem body* source for
transport-level errors until kinds removes it.

## 5. Frame format (WebSocket, WebTransport, NDJSON streams, mesh)

One `Frame` per WebSocket message (text for JSON, binary for postcard), per length-delimited
WebTransport/QUIC stream record (u32 big-endian length, then the codec bytes), per NDJSON line,
per mesh message. Maximum encoded size 64 KiB (C3; over → close 1009 / stream reset).

```json
{"id": 3, "kind": "call", "flags": 0, "tag": "Chat.room", "v": 1, "payload": {"room": "a"}}
{"id": 3, "kind": "item", "flags": 0, "cursor": "c1.AgAAAAAAAAAH", "payload": {"text": "hi"}}
{"id": 3, "kind": "exit", "flags": 0, "payload": {"_tag": "Success", "value": null}}
```

| Field | Type | Notes |
|---|---|---|
| `id` | u64 ≤ 2⁵³−1 | call id, chosen by the client, unique per connection while the call is open; `0` is the connection itself (`hello`, `resume`, `ping`) |
| `kind` | string (`FrameKind`) | below |
| `flags` | u8 bit set (`FrameFlags`) | bit 0 `END` (last item from this side: half-close), bit 1 `ACK` (the item needs a credit back), others reserved 0 |
| `tag`, `v` | on `call` only | as in the envelope |
| `cursor` | on server `item` frames of resumable calls | §6 |
| `credits` | u32, on `credit` | how many more items the sender of the credit accepts |
| `payload` | codec value | `call`: the input (or `null` for streams that send items); `item`: one item; `exit`: a `WireExit`; `hello`/`resume`: their objects |

`FrameKind` values (postcard: the variant index in this order, never reordered):

| # | `kind` | Direction | Meaning |
|---|---|---|---|
| 0 | `call` | c→s | open a call |
| 1 | `item` | both | one stream item (client stream / bidi from the client, streams and subscriptions from the server) |
| 2 | `end` | both | half-close without an item (`END` set) |
| 3 | `exit` | s→c | the call's final `WireExit`; closes the call |
| 4 | `cancel` | c→s | interrupt the call (`Cancelled`) |
| 5 | `credit` | both | grant `credits` more items (initial window 32) |
| 6 | `hello` | s→c, id 0 | `{"session": SessionId, "codec": "idrpc.json.v1", "max_frame": 65536, "window": 32}` |
| 7 | `resume` | c→s, id 0 | `{"session": SessionId, "calls": [{"id": 3, "cursor": "c1…"}]}`; the server replays from each cursor or answers `exit` `ResumeExpired` per call |
| 8 | `ping` / 9 `pong` | both, id 0 | application heartbeat where the transport has none (WebTransport, mesh); WebSocket uses its own ping (20 s, two missed close) |

Backpressure: a sender may have at most the granted credits of unacknowledged `item`s outstanding
per call; the server's streams are pull-based, so an exhausted window suspends the producer fiber
(a slow client slows its own stream, never the connection). Per-connection limits (streams, message
rate, idle) are the C3 limits of `id_effect_axum`'s realtime layer.

## 6. Cursor format and sessions

A cursor is an opaque string to clients; servers produce and parse it with `wire::Cursor`:

```
"c1." + base64url_nopad( source: u8 , position: u64 big-endian )   → 3 + 12 = 15 characters
```

- `source` 1 = an `id_effect_events` global position (subscriptions; `Cursor::events(pos)`),
  2 = a per-session message sequence (bidi/server-stream resume; `Cursor::sequence(n)`). Other
  values are refused.
- The decoder accepts at most 64 bytes of input and refuses padding, other prefixes and trailing
  data (fuzzed: `resume::decode_cursor`, SSE `Last-Event-ID`).
- A cursor is not a capability: it names a position, not a right. Resume authority comes from the
  `SessionId`, which is 128 random bits (`SessionId`, 22 base64url characters) **bound to the
  authenticated principal** of the connection that created it (C8); a resume under another
  principal is refused.
- Per-session replay buffer: 1,024 messages / 1 MiB / 30 s (configurable); overrun →
  `ResumeExpired`.

## 7. Codec negotiation (D13)

| Codec | HTTP media type | WebSocket subprotocol | NDJSON / stream framing |
|---|---|---|---|
| JSON (default) | `application/json` | `idrpc.json.v1` | NDJSON (`application/x-ndjson`), SSE `data:` lines |
| postcard | `application/vnd.id-effect.rpc+postcard` | `idrpc.postcard.v1` | u32-BE length-prefixed records (`application/vnd.id-effect.rpc+postcard; stream=1`) |

- HTTP: `Content-Type` picks the request codec (absent on a body-less GET means JSON); `Accept`
  picks the response codec (absent or `*/*` means the request codec). Unknown → **415** Problem.
- WebSocket: the client offers subprotocols; the server picks the first it knows; none known →
  the upgrade is refused (no silent JSON fallback). The `hello` frame repeats the choice.
- Mesh: postcard always.
- postcard decodes with serde, then runs the Schema's validators (refinements still apply).
- `codec::Codec` is the static trait (`const ID: CodecId`, `encode`, `decode`); `codec::CodecId`
  (`#[non_exhaustive]`) is the runtime choice and carries `name()`, `content_type()`,
  `subprotocol()`, `encode`, `decode`. r12-e12-1-postcard adds `CodecId::Postcard` (behind
  feature `postcard`) and its arm in each of those methods, in `codec/mod.rs`. CBOR and
  MessagePack are not offered.

## 8. Feature table (declared once in `crates/id_effect_rpc/Cargo.toml`)

| Feature | Enables | Leaf |
|---|---|---|
| `client` (default) | `client` module (`RpcClient` over `id_effect_platform` `HttpClientService`) | promote, batching |
| `postcard` | `codec::postcard` (`dep:postcard`) | postcard |
| `ws` | `transport::ws`, `client::ws` (axum `ws`, `tokio-tungstenite` — the major axum's `ws` uses) | rpc-ws-sse, bidi-resume |
| `sse` | `transport::sse`, `client::sse` | rpc-ws-sse |
| `events` | `subscription::events` (`dep:id_effect_events`) | subscriptions |
| `mesh` | `transport::mesh`, `client::mesh` (`dep:id_effect_node`, implies `postcard`) | mesh-rpc |
| `channels` | `channels` (implies `mesh`; `dep:id_effect_authz` for the `JoinPolicy` adapter) | channels |
| `http3` | HTTP/3 server mount and client (`id_effect_axum` / `id_effect_platform` `http3`) | h3 leaves |
| `webtransport` | `transport::webtransport` (implies `http3`, `ws` frame codec) | webtransport |
| `test-util` | in-process transport, fake clocks for transports, contract fixtures | every test leaf |

Later leaves add modules behind these features with one `pub mod` / `pub use` line in `lib.rs`
(append-only) and do not edit `Cargo.toml`. Dev-dependencies are declared too: `trybuild` (macro
ui tests), `proptest` (resume property), `id_effect_test` with `sql` and `id_effect_events` with
`pg` (subscription tests on their own container), `id_effect_platform` with `test-util`;
`channels` brings `id_effect_authz` (no features) for the `JoinPolicy` adapter. Not declared,
because the choosing leaf has not chosen yet: the D12 CDP driver (`chromiumoxide` or
`headless_chrome`, r12-e12-3-webtransport), which adds its one dev-dependency line.

## 9. Ownership notes for the coordinator

- `registry.rs` holds the handler storage (`RpcMethodEntry`, a boxed unary dispatch). The other five
  kinds need new handler shapes there, so r12-e12-1-kinds should own `registry.rs` in wave 2 (the
  release spec table lists it under promote only).
- `server/dispatch.rs` is transport-independent (`dispatch`, `authorize`, `Rejection`): every
  transport leaf calls it rather than re-implementing the guard order (outermost, i.e. last added,
  first).
- `protocol.rs` / `serialization.rs` (labs format) stay until kinds switches `server/http.rs` and
  `client/http.rs` to `wire`; kinds then deletes what is unused.

## 10. Coordinator review (2026-10-19) — approved with one amendment

Approved as the basis for r12-e12-1-kinds and the wave-2/3 leaves. `registry.rs` moves to kinds
(§9). Amendment, binding on r12-e12-1-batching / kinds where the idempotency store lands:

- **Idempotency on `.public()` mutations.** All anonymous callers share one principal, so
  `(principal, tag, key)` would let a second client that reuses a key with the same payload read a
  stored result that is not theirs. For a call with no authenticated principal the store keys on
  `(anonymous, client_ip, tag, key)`, and the key must be at least 16 characters; shorter keys on a
  public mutation are a `400` Problem `IdempotencyKeyTooShort`. Test:
  `idempotency_replay_is_scoped_for_anonymous_callers`.

Checked and kept: GET queries without `x-rpc` (side-effect free, response readable only under
CORS), 0-RTT for queries only (C7), batch charged N tokens, deadlines clamped server-side,
`InternalError` carries only the correlation id, sessions bound to the principal (C8), cursor
decoder bounded at 64 bytes.
