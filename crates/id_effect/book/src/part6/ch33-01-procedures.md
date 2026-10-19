# Procedures: the six kinds, typed errors, cancellation

A procedure is an effect. Whatever its kind, its handler returns an `Effect<_, E, DynEnv>`, so
the capabilities it needs come from the environment, its typed errors are part of its type, and
it can be tested with the test executor like any other effect. This chapter shows the low-level
`RpcGroup` API that `#[rpc::service]` generates against; each kind has one registration method
and one handler shape (module `id_effect_rpc::handler`).

| Kind | Register with | Handler returns |
|---|---|---|
| query | `RpcGroup::query` | `Effect<O, E, DynEnv>` from `I` |
| mutation | `RpcGroup::mutation` | `Effect<O, E, DynEnv>` from `I` |
| server stream | `RpcGroup::server_stream` | `Effect<Stream<O, E, DynEnv>, E, DynEnv>` from `I` |
| client stream | `RpcGroup::client_stream` | `Effect<O, E, DynEnv>` from a `Stream<I, E, DynEnv>` |
| two-way stream | `RpcGroup::bidi` | `Effect<Duplex<I, O, E>, E, DynEnv>` |
| subscription | `RpcGroup::subscription` | `Effect<Subscription<O, E>, E, DynEnv>` from `I` and a cursor |

Inputs and outputs cross the wire through a `WireSchema`: `WireSchema::of::<T>()` for a
`#[derive(Schema)]` type, or `WireSchema::new(schema)` for any Schema whose wire type is JSON-like.

```rust
use id_effect::{DynEnv, Never, Stream, fail, schema, succeed};
use id_effect_rpc::error_union::{RpcErrorStatus, RpcErrorUnion};
use id_effect_rpc::{Procedure, RpcGroup, RpcServer, WireSchema};
use axum::http::StatusCode;
use serde_json::{Value, json};

enum TodoError {
  NotFound { id: i64 },
}

impl RpcErrorStatus for TodoError {
  fn status(&self) -> StatusCode {
    StatusCode::NOT_FOUND
  }
}

impl RpcErrorUnion for TodoError {
  fn to_wire(&self) -> Value {
    match self {
      Self::NotFound { id } => json!({"_tag": "NotFound", "id": id}),
    }
  }
}

let mut group = RpcGroup::new();
group.query(
  Procedure::query("Todo.title").public(),
  WireSchema::new(schema::i64::<()>()),
  WireSchema::new(schema::string::<()>()),
  |id: i64, _env: &mut DynEnv| {
    if id == 1 { succeed("milk".to_owned()) } else { fail(TodoError::NotFound { id }) }
  },
);
group.server_stream(
  Procedure::server_stream("Todo.count").public(),
  WireSchema::new(schema::i64::<()>()),
  WireSchema::new(schema::i64::<()>()),
  |n: i64, _env: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
);
group.client_stream(
  Procedure::client_stream("Todo.sum").max_items(1_000).public(),
  WireSchema::new(schema::i64::<()>()),
  WireSchema::new(schema::i64::<()>()),
  |items: Stream<i64, Never, DynEnv>, _env: &mut DynEnv| items.run_fold(0, |a, b| a + b),
);
let app: axum::Router = RpcServer::new(group).mount_default().with_state(DynEnv::new());
# let _ = app;
```

## On the wire

Three routes serve every kind over HTTP:

- `POST /rpc` takes one envelope, `{"id": 7, "tag": "Todo.title", "payload": 1}`, and answers
  `{"id": 7, "tag": "Todo.title", "exit": {"_tag": "Success", "value": "milk"}}`.
- `POST /rpc/{tag}` takes the input as the body. A server stream answers NDJSON, one frame per
  line (`{"id":1,"kind":"item","flags":0,"payload":1}` … then one `exit` frame); a client stream
  sends NDJSON items (`Content-Type: application/x-ndjson`), capped by the procedure's
  `max_items` / `max_bytes` (10,000 items and 4 MiB by default) — over the cap the call ends with
  `ResourceExhausted`.
- `GET /rpc/{tag}?input=<url-encoded JSON>` serves queries with an `ETag`; a request with a
  matching `If-None-Match` gets `304 Not Modified`. Only queries are retry-safe, so only they are
  ever served by GET.

Two-way streams need a WebSocket or WebTransport session: over plain HTTP they answer `426
Upgrade Required`. Subscriptions are served over SSE and WebSocket (chapters 33.6–33.8). The
in-process transport (`kinds::InProcess`, feature `test-util`) calls every kind without a network,
through the same dispatch and guards — the way to test a two-way stream or a subscription today.

Every `POST` must carry `Content-Type: application/json` (or the stream's NDJSON) **and**
`x-rpc: 1`. A browser form cannot set either, so a cross-site form post can never call a
mutation (ADR 0013 R9): another content type answers `415`, a missing header `403`. The 0.11 CSRF
layer still applies to cookie sessions.

## Typed errors and defects

A typed error is a member of the procedure's error union: `{"_tag": "NotFound", "id": 2}` inside
`{"_tag": "Failure", "error": …}`, with the status the error declares (`RpcErrorStatus`, 400 by
default). The union's JSON Schema (`RpcErrorUnion::union_schema`) goes into the contract.

A defect — `die(..)`, a panic, an unexpected interrupt, or an error that claims one of the
reserved tags — never reaches the client as a message: it is logged with the request's
correlation id and answered as `{"_tag": "InternalError", "id": "<correlation id>"}` (500). The
other reserved failures are `InvalidInput { issues }` (422, every Schema issue at once, with JSON
Pointer paths), `DeadlineExceeded` (504), `Cancelled`, `ResourceExhausted { limit }` (429) and
`ResumeExpired` (410).

## Deadlines and cancellation

Each call runs under a deadline: the client's `x-rpc-deadline-ms`, never longer than the server's
`RpcServer::with_max_deadline` (30 s by default). When it expires, or the client disconnects, or a
multiplexed transport receives a `cancel` frame, the call's future is dropped. That interrupts the
handler fiber, and its finalizers run — `ensuring`, `acquire_use_release` and scoped layers clean
up exactly as they would on any other interrupt:

```rust,ignore
sync(|| started())
  .flat_map(|()| sleep(Duration::from_secs(3600)))
  .ensuring(sync(|| release_the_lock())) // runs when the client goes away
```

## Idempotent mutations

A mutation called with `Idempotency-Key: <key>` runs once: a retry with the same key and payload
gets the stored result (and `idempotency-replayed: true`), the same key with another payload is a
`409`. Keys belong to the caller: an authenticated principal (`RpcPrincipal`, or your own
`RpcServer::with_principal`) has its own; anonymous callers are scoped by client IP and need
keys of at least 16 characters. The default `MemoryIdempotencyStore` keeps results for 24 hours,
at most 100,000 keys; `IdempotencyStore` is the seam for a shared store.
