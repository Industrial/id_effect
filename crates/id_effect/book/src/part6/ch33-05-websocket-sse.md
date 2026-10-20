# WebSocket and server-sent events

`id_effect_axum::realtime` serves the two browser-native realtime transports as typed routes:
WebSocket (feature `ws`) for two-way messages and server-sent events (feature `sse`) for a
one-way event stream that resumes after a reconnect. Both follow the secure defaults of
ADR 0013: a route declares who may call it, the limits are on until you change them, and every
opt-out is named `dangerous_*` and recorded in the posture registry.

| Need | API |
|---|---|
| A WebSocket route | `realtime::ws::endpoint(path, WsConfig, handler)`, then `.public()` or `.guard(g)` |
| The handler | `Fn(&mut S, RequestCaps, WsSocket) -> Effect<(), E, S>` |
| Read and write | `socket.into_parts::<E, R>()` → `(Stream<WsMessage, E, R>, WsSender)`; `sender.send(msg)`, `sender.sink()` |
| Browser origins | `WsConfig::allowed_origins([...])`; `realtime::origin::DEV_ORIGINS` for the dev profile |
| Limits | `WsConfig::max_message_bytes`, `messages_per_second`, `max_sockets`, `heartbeat`, `idle_timeout`, `buffers` |
| Other transports, tests | `realtime::ws::serve_io(io, &config, handler)` on an already-upgraded connection |
| An SSE route | `realtime::sse::endpoint(path, SseConfig, handler)`, then `.public()` or `.guard(g)` |
| The SSE handler | `Fn(&mut S, RequestCaps, SseRequest) -> Effect<Stream<SseEvent, E2>, E, S>` |
| Resume | `SseRequest::last_event_id` (`LastEventId::as_u64`, `as_str`); `SseEvent::id` |
| SSE in any handler | `realtime::sse::response(events, &config)`; `sse::encode` for the bytes |

The runnable versions are
[`examples/090_ws_echo.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_axum/examples/090_ws_echo.rs)
and
[`examples/091_sse_ticker.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_axum/examples/091_sse_ticker.rs):

```bash
cargo run -p id_effect_axum --features ws --example 090_ws_echo -- --self-test
cargo run -p id_effect_axum --features sse --example 091_sse_ticker -- --self-test
```

## A WebSocket endpoint

A socket handler is an ordinary effect handler. It receives the state, the upgrade request's
`RequestCaps` (the principal an `Authenticate` layer found, the tenant) and the `WsSocket`:

```rust
use id_effect::{Effect, Never, succeed};
use id_effect_axum::RequestCaps;
use id_effect_axum::realtime::origin::DEV_ORIGINS;
use id_effect_axum::realtime::ws::{self, WsConfig, WsMessage, WsSocket};
use id_effect_axum::route::Router;

fn echo(_: &mut (), _caps: RequestCaps, socket: WsSocket) -> Effect<(), Never, ()> {
  let (incoming, out) = socket.into_parts::<Never, ()>();
  incoming.run_for_each_effect(move |msg| {
    let reply = match msg {
      WsMessage::Text(t) => WsMessage::Text(t.to_uppercase()),
      binary => binary,
    };
    out.send(reply).catch(|_closed| succeed(()))
  })
}

let config = WsConfig::default().allowed_origins(DEV_ORIGINS).unwrap();
let app: axum::Router = Router::new()
  .route(ws::endpoint("/echo", config, echo).public())
  .with_state(())
  .into_axum();
```

`ws::endpoint` returns an endpoint without an access decision, so `Router::route` does not
compile until you call `.public()` or `.guard(g)`, as for every typed route (chapter 29.1). The
guards and the `Authenticate` layer run on the upgrade request: an anonymous client gets its
401 before the `101 Switching Protocols`, never a socket.

### How a socket runs

Each accepted socket is one root fiber with two parts:

- the **driver** owns the connection: it reads frames into a bounded inbox, writes the outbox,
  sends pings, keeps the timers and enforces the limits;
- the **handler** runs as a child fiber. `incoming` is a pull-based `Stream`: each pull takes the
  queued messages (at least one), and a handler that stops pulling stops the reads once the
  inbox is full, which backpressures the client through TCP. `WsSender::send` suspends while the
  outbox is full, which backpressures the handler when the client reads slowly.

When the client goes away (or a limit or a timer closes the socket), the handler is
**interrupted** and its finalizers run, so a subscription it holds is released. When the handler
returns, the messages it queued are flushed and the socket closes with 1000 (1011 when the
handler failed or died). `serve_io` returns why the socket ended (`WsClose`).

### The defaults

| Default | Value | Above it |
|---|---|---|
| `Origin` allowlist | empty: every browser origin is refused | 403 Problem before the upgrade |
| Frame and message size | 64 KiB each | close 1009 |
| Inbound message rate | 100 per second per socket, bursts of 100 | close 1008 |
| Sockets | 16 per client IP, 10 000 per endpoint | 429 Problem with `Retry-After` |
| Heartbeat | a ping every 20 s; two unanswered pings close the socket | close 1001 |
| Idle timeout | 300 s without a message either way | close 1001 |
| Queues | 16 messages inbound, 16 outbound | backpressure |

Time is the executor clock, so on a `TestExecutor` the heartbeat and the timeouts run in virtual
time: the crate's tests drive a session over an in-memory pipe and `advance` the clock to see the
pings and the 1001 close without waiting a minute.

### Origins

A browser sends `Origin` with every WebSocket handshake and attaches the user's cookies whatever
page opened the socket. Without a check, any site the user visits could open an authenticated
socket to your service (cross-site WebSocket hijacking). So `WsConfig` holds an allowlist:

- exact origins (`https://app.example.com`, `https://app.example.com:8443`), compared without case
  and with the scheme's default port filled in, or an origin with any port
  (`http://localhost:*`);
- an **empty list refuses every browser origin** (the default);
- a request without `Origin` comes from a non-browser client (a CLI, another service) and is
  admitted; the guard still decides;
- `WsConfig::dangerous_any_origin()` admits every origin and records
  `id_effect_axum::realtime::origin::OriginPolicy::dangerous_any_origin` in the posture registry,
  which the production posture check reports.

The development profile allows the local origins (`DEV_ORIGINS`: `http://localhost:*`,
`http://127.0.0.1:*`, `http://[::1]:*`) **by configuration**: put them in the dev profile's
allowlist, not in code, and the production allowlist stays empty until you name your origins.

### Subprotocols

`WsConfig::protocols(["relay.v1", "relay.v0"])` lists the subprotocols the endpoint speaks in
preference order; the first one the client offers is selected, answered in
`Sec-WebSocket-Protocol` and available as `socket.protocol()`.

## Server-sent events

An SSE handler returns the stream of events to send. Each event may carry an id; a browser's
`EventSource` reconnects on its own and sends the last id it saw as `Last-Event-ID`, which the
handler receives as `SseRequest::last_event_id`:

```rust
use id_effect::{Effect, Never, Stream, succeed};
use id_effect_axum::RequestCaps;
use id_effect_axum::realtime::sse::{self, SseConfig, SseEvent, SseRequest};
use id_effect_axum::route::Router;

fn ticker(_: &mut (), _: RequestCaps, req: SseRequest) -> Effect<Stream<SseEvent>, Never, ()> {
  let from = req.last_event_id.and_then(|id| id.as_u64()).map_or(0, |n| n + 1);
  succeed(
    Stream::from_iterable(from..from + 3)
      .map(|n| SseEvent::data(n.to_string()).event("tick").id(n.to_string())),
  )
}

let app: axum::Router = Router::new()
  .route(sse::endpoint("/ticks", SseConfig::default(), ticker).public())
  .with_state(())
  .into_axum();
```

- `Last-Event-ID` is validated before the handler runs: at most 256 bytes of UTF-8 without CR, LF
  or NUL, sent once; anything else answers 400 Problem Details. An empty one means "from the
  start".
- Ids and event names never carry a line break (CR, LF and NUL are dropped from the field), and
  multi-line data becomes several `data:` lines, so an event cannot inject another.
- A heartbeat comment goes out after 15 s without an event (`SseConfig::keep_alive`), on the
  executor clock, so proxies keep the stream open and a dead client is found on the next write.
- `SseConfig::retry(d)` sends `retry:` first, the client's reconnection delay.
- Open streams are capped like sockets (16 per client IP, 10 000 per endpoint; 429 above).
- The events are pulled by a child fiber of the response's producer into a small buffer; when
  the client disconnects, the producer is interrupted and the stream's finalizers run.

To stream events from a handler that is not an SSE endpoint, return
`sse::response(events, &config)`.

## Testing a realtime handler

`serve_io` runs a WebSocket session on any upgraded byte stream. A `tokio::io::duplex` pipe and a
`TestExecutor` make the whole session deterministic: send a frame from the client half, call
`run_until_stalled`, advance the clock past the ping interval, and read what the server wrote.
`crates/id_effect_axum/tests/ws.rs` does this for the round trip, the heartbeat, the idle
timeout, the 1009 frame limit, the message rate and a client that drops the connection;
`tests/sse.rs` covers the resume, the heartbeat on the test clock and the stream cap.
