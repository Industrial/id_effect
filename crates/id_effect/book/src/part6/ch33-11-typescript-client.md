# A typed TypeScript client

`id-effect rpc gen ts` turns the service contract (`rpc.contract.json`, chapter 33.4) into a
typed TypeScript client: plain TypeScript on web platform APIs, with no npm package, no
`node_modules` and no runtime dependency.

```text
id-effect rpc gen ts --contract rpc.contract.json --out web/src/rpc
```

It writes three files, and leaves everything else in the directory alone:

| File | Holds |
|---|---|
| `types.ts` | the contract's types: every `$defs` definition, and per procedure `<Service>V<N><Proc>Input`, `…Output` and `…Error` |
| `client.ts` | one class per service and version (`TodosV1Client`, `GreeterV2Client`) with one method per procedure; it re-exports `runtime.ts` |
| `runtime.ts` | what the classes share: batching, transports, frame validation, reconnect and resume |

The output is deterministic (services by name and version, procedures and members by name,
nothing from the clock or the tool version), so it can be checked in and regenerated in CI. Each
file starts with `// biome-ignore-all format` and `// deno-fmt-ignore-file`, so formatters leave
it as generated.

## Types

Types come from the contract's JSON Schemas:

| Schema | TypeScript |
|---|---|
| object with `properties` | an object type; a member outside `required` is optional (`note?:`) |
| `Option<T>` (`anyOf [T, null]`) | `T \| null` |
| `Vec<T>` (`array`) | `T[]`; `prefixItems` is a tuple `[A, B]` |
| map (`additionalProperties`) | `{ [key: string]: V }` |
| tagged union (`oneOf` of `{_tag: const}` objects) | a union discriminated on `_tag` |
| `const`, `enum` | literal types |
| `integer`, `number` | `number` |
| a definition that is a scalar with `format: date-time` / `date` / `time` | an ISO 8601 `string` |
| any other scalar definition (a newtype such as `OrderId`) | a branded type: `string & { readonly __brand: "OrderId" }` |

Definitions are named like the OpenAPI components (chapter 33.10): after the last `::` segment of
their Rust type path, two paths that share it both taking the shortest suffix nobody else shares
(`a_Item`, `b_Item`), a TypeScript keyword getting a trailing `_`.

A procedure's error type is its typed error union; `RpcResult<A, E>` adds the reserved failures
every procedure may answer (`InternalError`, `InvalidInput`, `DeadlineExceeded`, `Cancelled`,
`ResourceExhausted`, `ResumeExpired`, `Rejected`):

```typescript
import { TodosV1Client } from "./rpc/client.ts";

const todos = new TodosV1Client({ url: "https://api.example.com/rpc" });
const result = await todos.get(1);
switch (result._tag) {
  case "Success":
    console.log(result.value.title);
    break;
  case "Failure":
    if (result.error._tag === "NotFound") console.log("no todo", result.error.id);
    break;
}
```

## Calls by kind

| Kind | Method | Returns |
|---|---|---|
| query, mutation | `get(input, options?)` | `Promise<RpcResult<O, E>>` |
| server stream | `upto(input, options?)` | `RpcStream<O, E>`: `for await` the items, `exit` the end |
| subscription | `ticks(input, { cursor })` | `RpcStream<O, E>`; its `cursor` resumes a later subscription |
| client stream | `sum(items, options?)` | `Promise<RpcResult<O, E>>`; `items` is any (async) iterable |
| two-way stream | `shout(options?)` | `RpcDuplex<I, O, E>`: `send(item)`, `end()`, `for await` the outputs |

A procedure whose input is `()` takes no input argument. Iterating a stream that fails throws
`RpcFailureError` (its `error` is the failure; the stream's `exit` resolves to it as well);
breaking out of the loop cancels the call.

Requests the server refuses before a procedure runs — a guard (401, 403), a full batch (413), a
rate limit (429), a reused `Idempotency-Key` (409) — reject with `RpcProblemError`, carrying the
RFC 9457 Problem. A network failure, a missing transport or a server that breaks the protocol
rejects with `RpcTransportError`.

## Batching

Queries and mutations made in the same microtask go out as **one** `POST {url}` with a JSON array
of calls, as `RpcClient::call` batches the calls of one tick on the Rust side:

```typescript
const [a, b, count] = await Promise.all([todos.get(1), todos.get(2), todos.count()]);
```

A batch holds at most 50 calls and 1 MiB (the server's limits; larger groups are split), calls
with different `deadlineMs` go in different requests, and `{ batch: false }` sends a call on its
own. A Problem answer (413, 429, 409, a guard's refusal) applies to the whole batch, so every call
of it rejects. Only queries and mutations are batched.

## Headers and idempotency

Every request carries `x-rpc: 1` (the CSRF header the server requires, C5). Every request with a
mutation carries an `Idempotency-Key` of 128 random bits (22 characters; anonymous callers need at
least 16), and a batch with more than one mutation carries `x-rpc-ordered: true`, so dependent
mutations run in call order. To make a retry replay a mutation instead of running it twice, pass
the key yourself:

```typescript
const key = crypto.randomUUID();
const placed = await shop.place(order, { idempotencyKey: key });
// … the connection dropped before the answer arrived: the same key replays the stored result.
const again = await shop.place(order, { idempotencyKey: key });
```

`headers` adds request headers (an `Authorization`, for example) and may be a function, called
for every request. Browsers cannot set headers on a WebSocket; the socket authenticates with the
page's cookies.

## Transports, reconnect and resume

Streams share one multiplexed connection per `RpcConnection`, chosen in this order:

1. **WebTransport**, when `webTransport: { url }` is configured and the platform has it;
2. **WebSocket** (`{url}/ws`, subprotocol `idrpc.json.v1`);
3. **server-sent events** for server streams and subscriptions (`GET {url}/{tag}`), with queries
   and mutations over `POST` as always. Client streams and two-way streams need one of the first
   two.

Every frame from the server is checked before the client acts on it — size (64 KiB), kind,
flags, the members each kind carries, the `WireExit` of an `exit`, the `hello` — and a server that
breaks a rule loses the connection. Credits flow both ways: the client sends a two-way or
client stream's items only while it holds credits, and grants the server credits back as the
program reads, so a slow reader slows its own stream.

When the connection drops, the client reconnects with exponential backoff and jitter (250 ms,
doubling to 10 s, ten attempts; `reconnect` changes them) and:

- **resumes** server streams and two-way streams with a `resume` frame naming the session and the
  last cursor of each call: the server replays what came after it, so every item arrives once
  and in order. When the server no longer holds them, the stream ends with `ResumeExpired`;
  restart it if that is safe. Items a two-way stream sent while the socket was down are not
  replayed (input is at most once across a reconnect, chapter 33.7);
- **reopens** subscriptions after their last event cursor (SSE: `Last-Event-ID`);
- fails client streams with `RpcTransportError` (they are not resumable).

`AbortSignal` cancels any call: a query still waiting for its batch never leaves, one in flight
on its own aborts its request, and a stream sends a `cancel` frame. The promise or the stream
rejects with the signal's reason.

```typescript
const controller = new AbortController();
setTimeout(() => controller.abort(), 1000);
for await (const tick of todos.ticks(0, { signal: controller.signal })) render(tick);
```

## Sharing a connection

Each client class takes either options or an `RpcConnection`; clients built on one connection
share its batches and its frame connection:

```typescript
import { GreeterV2Client, RpcConnection, TodosV1Client } from "./rpc/client.ts";

const connection = new RpcConnection({ url: "/rpc", headers: () => ({ authorization: token() }) });
const todos = new TodosV1Client(connection);
const greeter = new GreeterV2Client(connection);
```

## Testing the generated client

The client's own tests (`crates/id_effect_tool/tests/ts`) run under Deno, pinned by devenv and
CI, against a real server on loopback: `deno check` type-checks the generated files and the tests,
and `deno test` runs every kind, batching, the SSE fallback, cancellation and resume after a cut
socket. `cargo nextest run -p id_effect_tool --test rpc_gen_ts` starts the server and runs them.
