# Batching: many calls, one request

A page that needs ten small reads should not pay for ten round trips. `POST /rpc` takes a JSON
**array** of calls as well as a single call, runs them all and answers with an array of results
in the same order:

```json
[{"id": 1, "tag": "User.name", "payload": 7},
 {"id": 2, "tag": "User.name", "payload": 9},
 {"id": 3, "tag": "Todo.add", "payload": "milk"}]
```

```json
[{"id": 1, "tag": "User.name", "exit": {"_tag": "Success", "value": "Ada"}},
 {"id": 2, "tag": "User.name", "exit": {"_tag": "Failure", "error": {"_tag": "NotFound", "id": 9}}},
 {"id": 3, "tag": "Todo.add", "exit": {"_tag": "Success", "value": 41}}]
```

Every call needs an `id` no other call of the batch uses; it is echoed in its result. A batch is
always answered `200`: each call succeeds or fails on its own, with the exit a single call would
have had.

Batches are JSON only: with the postcard media type, `POST /rpc` carries one postcard call
envelope (postcard has no array marker a server could tell from a call).

## What a batch may carry

Only **queries and mutations**. A stream, a two-way stream or a subscription in a batch — or a
tag the server does not serve — is that call's `InvalidInput` failure, with an issue at `/tag`;
the other calls still run.

## Limits, checked before anything runs

| Rule | Default | Over it |
|---|---|---|
| Calls per batch | 50 (`wire::MAX_BATCH_CALLS`) | `413` Problem, code `BatchTooManyCalls` |
| Bytes per batch | 1 MiB (`wire::MAX_BATCH_BYTES`) | `413` Problem, code `BatchTooLarge` |
| Ids | present and distinct | `400` Problem |

The call count is checked before a single element is decoded, so an oversized batch costs the
server a scan, not fifty decodes. Configure the limits per server:

```rust
use id_effect_rpc::batch::BatchConfig;
use id_effect_rpc::{RpcGroup, RpcServer};

let server = RpcServer::new(RpcGroup::new())
  .with_batch_config(BatchConfig::new().with_max_calls(20).with_parallelism(4));
assert_eq!(server.batch_config().max_calls(), 20);
```

## Access, rate limits, idempotency

- **Every call is authorized** by its own guards before any call of the batch runs. If one
  guard refuses, the whole batch gets that guard's response (with `x-rpc-batch-index` naming
  the call), so no call runs beside a refused one.
- **A batch of N calls costs N rate-limit tokens.** `RateLimitLayer` takes one token for the
  request and leaves a `RateLimitCharge` in the request's extensions; the batch takes the other
  N − 1 from the same bucket, all or none, or answers the layer's `429` with `Retry-After`. A
  batch larger than the bucket's burst can never pass.
- **Mutations keep `Idempotency-Key`.** With the header, each mutation of the batch is keyed by
  `<key>#<call id>` in the usual scope (the principal; for anonymous callers the client IP and a
  key of at least 16 characters). Retrying the same batch replays every mutation that finished;
  reusing the key with another payload, or while it runs, is a `409` Problem and nothing runs.

## Concurrency and ordering

The calls run in parallel, bounded by the server's compute admission: an `AdmissionController`
with 8 permits by default, shared by every batch of the server (`BatchConfig::with_admission`
plugs in a controller a `FabricGovernor` rebalances). With the header `x-rpc-ordered: true`
they run one after the other, in array order — for a batch whose mutations depend on each
other. Either way the request's deadline (`x-rpc-deadline-ms`, clamped to the server maximum)
bounds the whole batch, waiting for admission included; a call it cuts short answers
`DeadlineExceeded`.

## Deduplicated loads: one `RequestCache` per batch

Each batch runs its calls with a fresh `id_effect::RequestCache`. When several calls load the
same thing through a `RequestResolver`, the resolver sees the key once — the DataLoader pattern,
with nothing shared between batches (or between users).

```rust
use id_effect::{DynEnv, Effect, Never, Request, RequestResolver, schema, sync};
use id_effect_rpc::{Procedure, RpcGroup, WireSchema};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GetUser(i64);

impl Request for GetUser {
  type Output = String;
  type Error = Never;
}

let users: RequestResolver<GetUser, DynEnv> = RequestResolver::make_batched(|reqs| {
  // One query for every distinct id the batch asked for.
  sync(move || reqs.into_iter().map(|GetUser(id)| Ok(format!("user-{id}"))).collect())
});
let mut group = RpcGroup::new();
group.query(
  Procedure::query("User.name").public(),
  WireSchema::new(schema::i64::<()>()),
  WireSchema::new(schema::string::<()>()),
  move |id: i64, _env: &mut DynEnv| Effect::request(GetUser(id), &users),
);
```

A batch `[User.name 1, User.name 2, User.name 1]` reaches the resolver with the keys `1` and
`2`, once each.

## The Rust client batches for you

`RpcClient::call` collects the calls a program makes in the same tick — the children of one
`all`, say — and sends them as one batch (at most 50 calls and 1 MiB per request; more become
several requests). A single call in its tick goes out as a plain envelope. Each call still gets
its own typed result: `all([client.call(..), client.call(..)], Concurrency::Unbounded)` is one
HTTP request carrying two calls. `RpcClient::call_unbatched` sends a call at once, on its own.
