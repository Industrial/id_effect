# RPC transports: WebSocket and server-sent events

A procedure is written once and served on every transport. Chapter 33.1 served the six kinds
over HTTP: unary calls as JSON, server and client streams as NDJSON. This chapter adds the two
transports a browser needs for long-lived streams:

| Transport | Route | Kinds | Feature |
|---|---|---|---|
| WebSocket | `GET /rpc/ws` | all six, multiplexed on one socket | `ws` |
| Server-sent events | `GET /rpc/{tag}` with `Accept: text/event-stream` | subscriptions, server streams | `sse` |

Both run each call through the same dispatch as HTTP. Guards, Schema decoding, typed errors and
cancellation work the same way on every transport. The connection limits come from
`id_effect_axum`'s realtime layer (chapter 33.5): connections per client IP and per endpoint,
the 64 KiB frame cap, message rate, heartbeats and the idle timeout.

## Serving

```rust,ignore
use id_effect_axum::realtime::ws::WsConfig;
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::RpcServer;

let socket = WsConfig::default().allowed_origins(["https://app.example.com"])?;
let router = RpcServer::new(group)
  .with_websocket(RpcWsConfig::default().socket(socket).max_calls(100))
  .router();
```

`with_websocket` adds `GET /rpc/ws`. The route itself is public, and every call runs its own
procedure's guards against the head of the upgrade request. That head includes the cookies and
`Authorization` header the browser sent, and whatever an `Authenticate` layer put there. Nothing
is open until you decide it:

- **Origins.** The allowlist is empty by default, so every browser origin is refused. Clients
  that send no `Origin` header, such as the Rust client, are let through. The development profile
  lists `http://localhost:*` in configuration.
- **Codec.** The client offers subprotocols, `idrpc.json.v1` (and `idrpc.postcard.v1` when the
  codec is built). If it offers none of them, the upgrade gets **415** `UnsupportedSubprotocol`.
  The server never falls back to JSON on its own.
- **Calls per connection.** At most 100 calls are open at once on one socket (C3). A call
  beyond that gets an `exit` frame carrying `ResourceExhausted { limit: "calls" }`.
- **Message rate.** The realtime layer allows 100 inbound messages per second per socket, and
  every frame counts, credits included. A client that streams items faster than that needs a
  higher `WsConfig::messages_per_second`.

Server-sent events need no extra route. With the `sse` feature, `GET /rpc/{tag}` on a
subscription or server stream answers an event stream when the request has
`Accept: text/event-stream`. `RpcServer::with_sse(SseConfig)` sets the stream caps: 16 per client
IP and 10,000 in total by default, then **429**. It also sets the 15 s heartbeat.

## Frames on the socket

Every message is one frame (design note §5). The server greets the client first:

```json
{"id": 0, "kind": "hello", "payload": {"session": "…22 chars…", "codec": "idrpc.json.v1", "max_frame": 65536, "window": 32}}
```

The client picks the call ids. A `call` frame opens a call, the server answers with `item`
frames and then exactly one `exit` frame:

```json
{"id": 3, "kind": "call", "tag": "Chat.room", "payload": {"room": "a"}}
{"id": 3, "kind": "item", "cursor": "c1.AQAAAAAAAAAH", "payload": {"text": "hi"}}
{"id": 3, "kind": "exit", "payload": {"_tag": "Success", "value": null}}
```

- A client stream or two-way stream sends its input as `item` frames. An `end` frame, or the
  `END` flag on the last item, closes the input.
- A `cancel` frame interrupts the call. The handler's finalizers run, and the exit is
  `Cancelled`.
- A call refused before it ran, because its tag is unknown or a guard said no, exits with
  `{"_tag": "Rejected", "status": 404 | 403 | …}`.
- A subscription's `call` frame may carry the `cursor` to resume after.
- Some frames close the socket: a frame that does not decode
  (`id_effect_rpc::transport::frame::decode` is the fuzzed boundary), a `call` on an id that is
  already open, or an `exit` or `hello` sent by the client.

### Backpressure

Streams are pull-based end to end, and credits carry that across the socket. Each side may have
only as many `item`s outstanding per call as it holds credits. Each side starts with the window
(32) and gets more from `credit` frames.

When a server stream has used its credits, the server stops pulling it. Its producer then
suspends behind a bounded window, so a slow reader slows down only its own stream. Other calls on
the same socket keep running. In the other direction, the server gives credits back as the
handler takes items, half a window at a time. A client that sends past its credits fails that
call with `ResourceExhausted { limit: "credits" }`.

Queries, mutations and client streams run under the server's deadline cap (30 s by default). The
other streams run until they end, until they are cancelled, or until the socket closes. Closing
the socket interrupts every call still open on it, except server streams and two-way streams,
which wait for a resume (next chapter).

### Sessions

Every connection gets a session: 128 random bits bound to the connection's principal (C8),
announced in `hello`. Resuming streams after a reconnect is the topic of the next chapter
([two-way streams and resume](./ch33-07-two-way-streams.md)).

## Events next to POST

Server-sent events are the browser fallback. `EventSource` subscribes, and mutations keep going
over `POST`:

```text
GET /rpc/Chat.room?input=%7B%22room%22%3A%22a%22%7D
Accept: text/event-stream

event: item
data: {"text":"hi"}
id: c1.AQAAAAAAAAAH

event: exit
data: {"_tag":"Success","value":null}
```

An item's cursor becomes the event `id:`. When an `EventSource` reconnects, it sends the last id
it saw as `Last-Event-ID`, and the subscription resumes after it. The header must be a valid
cursor; anything else is a **400**. Every reconnect is a new request, so the guards run again.

Close the `EventSource` on the `exit` event. If you don't, it reconnects and calls again. Like a
query on `GET`, a procedure served this way must not change state: a cross-site page can start
the request, even though it cannot read the answer.

## The Rust client

`RpcClientConfig::transport` picks the stream transport, and `RpcClient::subscribe` uses it:

```rust,ignore
use id_effect_rpc::client::{CallEvent, RpcClient, TransportKind};

let client = RpcClient::new("https://api.example.com").with_transport(TransportKind::Sse);
let stream = client.subscribe("Chat.room", json!({"room": "a"}), None); // Effect<RpcStream, …>
// stream.next() yields CallEvent::Item { value, cursor } … then CallEvent::Exit(result)
```

To multiplex calls on one socket, use `client::ws::WsClient` directly:

```rust,ignore
let ws = WsClient::connect("ws://127.0.0.1:8080/rpc/ws").await?;
let greeting = ws.call("Greeter.greet", json!("Ada")).await?;
let mut feed = ws.open("Feed.watch", json!("a"), None).await?;
while let Some(event) = feed.next().await { /* … */ }
```

Reading items grants the server credits. Dropping a `WsCall` cancels it on the server.

Clients generated by `#[rpc::service]` take any `service::RpcTransport`. `WsClient` is one that
carries every kind: `TodosService::client(WsClient::connect(url).await?)`. `client::sse::SseTransport`
carries subscriptions and server streams as events, and queries and mutations over `POST /rpc`.
On these transports a refused stream call ends with its `exit` frame, because the stream is
returned before the server answers; a refused query or mutation is `TransportError::Refused`.

`client::sse` goes through `HttpClientService`, so the platform client's SSRF guard and timeouts
apply. For a long subscription, set a longer timeout and resume from `SseCall::last_cursor`.
